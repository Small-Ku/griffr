use std::io::ErrorKind;
use std::path::Path;

use crate::error::{Error, Result};
use compio::buf::BufResult;
use compio::bytes::Bytes;
use compio::dispatcher::Dispatcher;
use compio::io::AsyncWriteAtExt;
use futures_util::StreamExt;
use md5::{Digest, Md5};
use tracing::debug;

const DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS: u64 = 15 * 60;

fn duration_from_env_secs(var: &str, default_secs: u64) -> std::time::Duration {
    let secs = std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs);
    std::time::Duration::from_secs(secs)
}

pub(crate) fn do_download(
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    url: &str,
    dest: &Path,
    expected_md5: &str,
    expected_size: Option<u64>,
    progress_buffer_bytes: usize,
    on_progress: Option<impl Fn(u64) + Send + 'static>,
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
    let part_path_for_resume = part_path.clone();
    let resume_from = super::fs_ops::dispatch_io(io_dispatcher, move || async move {
        match compio::fs::metadata(&part_path_for_resume).await {
            Ok(metadata) => Ok::<Option<u64>, Error>(match expected_size {
                Some(size) if metadata.len() < size => Some(metadata.len()),
                Some(_) => Some(0),
                None => Some(metadata.len()),
            }),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::StatFailed {
                path: part_path_for_resume.clone(),
                source: err,
            }),
        }
    })?;

    let url_owned = url.to_string();
    let user_agent_owned = user_agent.to_string();
    let part_path_for_write = part_path.clone();
    let (written, actual_md5) = super::fs_ops::dispatch_io(io_dispatcher, move || async move {
        let client = cyper::Client::new();
        let mut request = client.get(&url_owned)?;
        request = request
            .header("User-Agent", user_agent_owned)
            .map_err(|e| Error::Download(format!("Failed to attach User-Agent header: {e}")))?;
        if let Some(offset) = resume_from.filter(|o| *o > 0) {
            request = request
                .header("Range", format!("bytes={}-", offset))
                .map_err(|e| {
                    Error::Download(format!("Failed to set Range header for resume: {e}"))
                })?;
            debug!("resuming download from byte {} for {}", offset, url_owned);
        }
        let response = compio::time::timeout(send_timeout, request.send())
            .await?
            .map_err(|e| Error::Download(format!("Failed to download {}: {e}", url_owned)))?;
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

        let resume_effective = resume_from.filter(|o| *o > 0).is_some() && status.as_u16() == 206;
        let resume_offset = resume_from.unwrap_or(0);
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

        let mut hasher = Md5::new();
        if resume_effective {
            super::fs_ops::hash_file_prefix_into_hasher(
                &part_path_for_write,
                resume_offset,
                &mut hasher,
            )?;
        }

        let mut stream = response.bytes_stream();
        let mut total_written = if resume_effective { resume_offset } else { 0 };
        let mut write_offset = total_written;
        let mut last_reported_bytes = total_written;
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
            if let Some(ref cb) = on_progress {
                if total_written - last_reported_bytes >= progress_buffer_bytes as u64 {
                    cb(total_written);
                    last_reported_bytes = total_written;
                }
            }
        }

        if let Some(ref cb) = on_progress {
            if total_written > last_reported_bytes {
                cb(total_written);
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

    if actual_md5 != expected_md5.to_lowercase() {
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
