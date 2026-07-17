use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::api::protocol::{byte_range_from, RANGE_HEADER, USER_AGENT_HEADER};
use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
use compio::buf::BufResult;
use compio::bytes::Bytes;
use compio::dispatcher::Dispatcher;
use compio::io::AsyncWriteAtExt;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use tracing::debug;

use super::types::DownloadResumeState;

const DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS: u64 = 15 * 60;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

fn duration_from_env_secs(var: &str, default_secs: u64) -> std::time::Duration {
    let secs = std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs);
    std::time::Duration::from_secs(secs)
}

pub(crate) enum DownloadPreparation {
    Complete(u64),
    Ready(DownloadResumeState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadProgress {
    Advanced(u64),
    Reset(u64),
}

/// Inspects a partial download and computes the incremental MD5 prefix on a
/// CPU worker before the network task is admitted to an I/O queue.
pub(crate) fn prepare_download(
    io_dispatcher: Option<&Dispatcher>,
    dest: &Path,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Result<DownloadPreparation> {
    let part_path = super::fs_ops::make_partial_download_path(dest)?;
    let metadata = match std::fs::metadata(&part_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            return Ok(DownloadPreparation::Ready(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
        Err(source) => {
            return Err(Error::StatFailed {
                path: part_path,
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(Error::Download(format!(
            "Partial download path is not a file: {}",
            part_path.display()
        )));
    }

    let partial_len = metadata.len();
    if let Some(expected_size) = expected_size {
        if partial_len > expected_size {
            std::fs::remove_file(&part_path).map_err(|source| Error::RemoveFailed {
                path: part_path.clone(),
                source,
            })?;
            return Ok(DownloadPreparation::Ready(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
        if partial_len == expected_size {
            let actual_md5 = super::verify::file_md5(&part_path)?;
            if actual_md5 == expected_md5.to_ascii_lowercase() {
                super::fs_ops::commit_partial_download(io_dispatcher, &part_path, dest)?;
                return Ok(DownloadPreparation::Complete(partial_len));
            }
            std::fs::remove_file(&part_path).map_err(|source| Error::RemoveFailed {
                path: part_path.clone(),
                source,
            })?;
            return Ok(DownloadPreparation::Ready(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
    }

    let mut hasher = Md5::new();
    super::fs_ops::hash_file_prefix_into_hasher(&part_path, partial_len, &mut hasher)?;
    Ok(DownloadPreparation::Ready(DownloadResumeState::new(
        partial_len,
        hasher,
    )))
}

pub(crate) fn do_prepared_download(
    io_dispatcher: Option<&Dispatcher>,
    http_client: &cyper::Client,
    user_agent: &str,
    url: &str,
    dest: &Path,
    expected_md5: &str,
    expected_size: Option<u64>,
    resume: DownloadResumeState,
    progress_buffer_bytes: usize,
    on_progress: Option<impl Fn(DownloadProgress) + Send + 'static>,
) -> Result<u64> {
    let send_timeout = duration_from_env_secs(
        "GRIFFR_DOWNLOAD_SEND_TIMEOUT_SECS",
        DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS,
    );
    let body_timeout = duration_from_env_secs(
        "GRIFFR_DOWNLOAD_BODY_TIMEOUT_SECS",
        DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS,
    );
    let part_path = super::fs_ops::make_partial_download_path(dest)?;
    let resume_offset = resume.offset;
    let prepared_hasher = resume.take_hasher();
    let url_owned = url.to_string();
    let user_agent_owned = user_agent.to_string();
    let part_path_for_write = part_path.clone();
    let client = http_client.clone();
    let (written, actual_md5) = super::fs_ops::dispatch_io(io_dispatcher, move || async move {
        let mut request = client.get(&url_owned)?;
        request = request
            .header(USER_AGENT_HEADER, user_agent_owned.clone())
            .map_err(|e| Error::Download(format!("Failed to attach User-Agent header: {e}")))?;
        if resume_offset > 0 {
            request = request
                .header(RANGE_HEADER, byte_range_from(resume_offset))
                .map_err(|e| {
                    Error::Download(format!("Failed to set Range header for resume: {e}"))
                })?;
            debug!(
                "resuming download from byte {} for {}",
                resume_offset, url_owned
            );
        }
        let mut response = compio::time::timeout(send_timeout, request.send())
            .await?
            .map_err(|e| Error::Download(format!("Failed to download {}: {e}", url_owned)))?;
        let mut progress_reset = false;
        if resume_offset > 0 && response.status().as_u16() == 416 {
            match compio::fs::remove_file(&part_path_for_write).await {
                Ok(()) => {}
                Err(source) if source.kind() == ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::RemoveFailed {
                        path: part_path_for_write.clone(),
                        source,
                    })
                }
            }
            if let Some(ref callback) = on_progress {
                callback(DownloadProgress::Reset(0));
            }
            progress_reset = true;
            debug!(
                "server rejected resume offset {}; restarting {} from byte zero",
                resume_offset, url_owned
            );

            let retry_request = client
                .get(&url_owned)?
                .header(USER_AGENT_HEADER, user_agent_owned.clone())
                .map_err(|e| Error::Download(format!("Failed to attach User-Agent header: {e}")))?;
            response = compio::time::timeout(send_timeout, retry_request.send())
                .await?
                .map_err(|e| {
                    Error::Download(format!(
                        "Failed to restart download {} after HTTP 416: {e}",
                        url_owned
                    ))
                })?;
        }

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Download(format!("HTTP error {}", status)));
        }

        if let Some(parent) = part_path_for_write.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }

        let resume_effective = resume_offset > 0 && status.as_u16() == 206;
        if resume_offset > 0 && !resume_effective && !progress_reset {
            if let Some(ref callback) = on_progress {
                callback(DownloadProgress::Reset(0));
            }
            debug!(
                "server ignored resume range at byte {}; restarting {} from byte zero",
                resume_offset, url_owned
            );
        }
        let mut open_options = compio::fs::OpenOptions::new();
        open_options
            .create(true)
            .write(true)
            .truncate(!resume_effective);
        let mut out =
            open_options
                .open(&part_path_for_write)
                .await
                .map_err(|e| Error::OpenFileFailed {
                    path: part_path_for_write.clone(),
                    source: e,
                })?;

        if let Some(expected_size) = expected_size {
            preallocate_file(&out, &part_path_for_write, expected_size)?;
        }

        let mut hasher = if resume_effective {
            prepared_hasher
        } else {
            Md5::new()
        };
        let mut stream = response.bytes_stream();
        let mut total_written = if resume_effective { resume_offset } else { 0 };
        let mut write_offset = total_written;
        let mut last_reported_bytes = total_written;
        let mut last_reported_at = Instant::now();
        let progress_threshold = (progress_buffer_bytes as u64).max(1);
        loop {
            let next: Option<std::result::Result<Bytes, cyper::Error>> =
                compio::time::timeout(body_timeout, stream.next())
                    .await
                    .map_err(|_| {
                        Error::Download(format!(
                            "Timed out reading response body from {} (timeout={}s)",
                            url_owned,
                            body_timeout.as_secs()
                        ))
                    })?;
            let Some(chunk) = next else {
                break;
            };
            let chunk: Bytes = chunk
                .map_err(|e| Error::Download(format!("Failed to read response body chunk: {e}")))?;
            md5::Digest::update(&mut hasher, chunk.as_ref());
            let chunk_len = chunk.len() as u64;
            let BufResult(write_result, _) = out.write_all_at(chunk, write_offset).await;
            write_result.map_err(|e| Error::WriteFileFailed {
                path: part_path_for_write.clone(),
                source: e,
            })?;
            write_offset = write_offset.saturating_add(chunk_len);
            total_written = total_written.saturating_add(chunk_len);
            if let Some(ref callback) = on_progress {
                let byte_threshold_reached =
                    total_written.saturating_sub(last_reported_bytes) >= progress_threshold;
                if byte_threshold_reached || last_reported_at.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    callback(DownloadProgress::Advanced(total_written));
                    last_reported_bytes = total_written;
                    last_reported_at = Instant::now();
                }
            }
        }

        if let Some(ref callback) = on_progress {
            if total_written > last_reported_bytes {
                callback(DownloadProgress::Advanced(total_written));
            }
        }

        out.sync_data().await.map_err(|e| Error::WriteFileFailed {
            path: part_path_for_write.clone(),
            source: e,
        })?;

        if let Some(expected) = expected_size {
            if total_written != expected {
                return Err(Error::Download(format!(
                    "Downloaded size mismatch for {}: expected {}, got {}",
                    url_owned, expected, total_written
                )));
            }
        }

        let actual_md5 = format!("{:x}", md5::Digest::finalize(hasher));
        Ok::<(u64, String), Error>((total_written, actual_md5))
    })?;

    if actual_md5 != expected_md5.to_ascii_lowercase() {
        return Err(Error::Download(format!(
            "MD5 mismatch: expected {}, got {}",
            expected_md5, actual_md5
        )));
    }

    super::fs_ops::commit_partial_download(io_dispatcher, &part_path, dest)?;
    let dest_owned = dest.to_path_buf();
    let metadata = super::fs_ops::dispatch_io(io_dispatcher, move || async move {
        compio::fs::metadata(&dest_owned)
            .await
            .map_err(|e| Error::StatFailed {
                path: dest_owned.clone(),
                source: e,
            })
    })?;
    let len = metadata.len();
    if len != written {
        debug!(
            "download committed with metadata size {} differing from streamed size {} for {}",
            len, written, url
        );
    }
    Ok(len)
}
