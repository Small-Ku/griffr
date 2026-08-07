use std::io::ErrorKind;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::api::protocol::{byte_range_from, RANGE_HEADER, USER_AGENT_HEADER};
use crate::error::{Error, Result};
use crate::runtime::{
    preallocate_file, ArtifactDigest, ArtifactExpectation, ArtifactProof, ArtifactSource,
};
use md5::{Digest, Md5};
use tracing::debug;

use super::types::DownloadResumeState;

const DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS: u64 = 15 * 60;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(100);

fn duration_from_env_secs(var: &str, default_secs: u64) -> Duration {
    let secs = std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs);
    Duration::from_secs(secs)
}

pub(crate) fn download_timeouts() -> (Duration, Duration) {
    static TIMEOUTS: OnceLock<(Duration, Duration)> = OnceLock::new();
    *TIMEOUTS.get_or_init(|| {
        (
            duration_from_env_secs(
                "GRIFFR_DOWNLOAD_SEND_TIMEOUT_SECS",
                DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS,
            ),
            duration_from_env_secs(
                "GRIFFR_DOWNLOAD_BODY_TIMEOUT_SECS",
                DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS,
            ),
        )
    })
}

pub(crate) enum DownloadPreparation {
    Done(ArtifactProof),
    Resume(DownloadResumeState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadProgress {
    Advanced(u64),
    Reset(u64),
}

/// Result of one bounded streaming download that was verified and discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamingDownloadReport {
    /// Number of bytes verified before the temporary destination was removed.
    pub bytes: u64,
    /// Wall-clock duration of the download and verification.
    pub elapsed: Duration,
}

/// Inspects a partial download and computes the incremental MD5 prefix in a
/// CPU admission before the async transfer task is submitted to Dispatcher.
pub(crate) fn prepare_download(
    dest: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Result<DownloadPreparation> {
    let part_path = super::fs_ops::make_partial_download_path(dest)?;
    let metadata = match std::fs::metadata(&part_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            return Ok(DownloadPreparation::Resume(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
        Err(source) => {
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
                path: part_path,
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!(
                "Partial download path is not a file: {}",
                part_path.display()
            ),
        });
    }

    let partial_len = metadata.len();
    if let Some(expected_size) = expected_size {
        if partial_len > expected_size {
            std::fs::remove_file(&part_path).map_err(|source| Error::IoAt {
                action: "remove file or directory",
                path: part_path.clone(),
                source,
            })?;
            return Ok(DownloadPreparation::Resume(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
        if partial_len == expected_size {
            let actual_md5 = super::verify::file_md5(&part_path)?;
            if actual_md5.eq_ignore_ascii_case(expected_md5) {
                let expectation =
                    ArtifactExpectation::new(logical_path, expected_md5, Some(expected_size));
                let digest = ArtifactDigest::new(partial_len, actual_md5);
                let proof = super::fs_ops::commit_observed_artifact(
                    &part_path,
                    dest,
                    &expectation,
                    ArtifactSource::Download,
                    &digest,
                )?;
                return Ok(DownloadPreparation::Done(proof));
            }
            std::fs::remove_file(&part_path).map_err(|source| Error::IoAt {
                action: "remove file or directory",
                path: part_path.clone(),
                source,
            })?;
            return Ok(DownloadPreparation::Resume(DownloadResumeState::new(
                0,
                Md5::new(),
            )));
        }
    }

    let mut hasher = Md5::new();
    super::fs_ops::hash_file_prefix_into_hasher(&part_path, partial_len, &mut hasher)?;
    Ok(DownloadPreparation::Resume(DownloadResumeState::new(
        partial_len,
        hasher,
    )))
}

pub(crate) async fn do_prepared_download(
    user_agent: &str,
    url: &str,
    dest: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
    resume: DownloadResumeState,
    progress_buffer_bytes: usize,
    on_progress: Option<impl Fn(DownloadProgress) + Send + 'static>,
) -> Result<ArtifactProof> {
    let (send_timeout, body_timeout) = download_timeouts();
    let part_path = super::fs_ops::make_partial_download_path(dest)?;
    let resume_offset = resume.offset;
    let prepared_hasher = resume.take_hasher();
    let url_owned = url.to_string();
    let user_agent_owned = user_agent.to_string();
    let part_path_for_write = part_path.clone();
    let (written, actual_md5) = {
        thread_local! {
            static CLIENT: cyper::Client = cyper::Client::new().expect("Failed to create thread-local HTTP client");
        }
        let client = CLIENT.with(|c| c.clone());
        let mut request = client.get(&url_owned)?;
        request = request
            .header(USER_AGENT_HEADER, user_agent_owned.clone())
            .map_err(|e| Error::Message {
                context: "Download error: ",
                detail: format!("Failed to attach User-Agent header: {e}"),
            })?;
        if resume_offset > 0 {
            request = request
                .header(RANGE_HEADER, byte_range_from(resume_offset))
                .map_err(|e| Error::Message {
                    context: "Download error: ",
                    detail: format!("Failed to set Range header for resume: {e}"),
                })?;
            debug!(
                "resuming download from byte {} for {}",
                resume_offset, url_owned
            );
        }
        let mut response = compio::time::timeout(send_timeout, request.send())
            .await?
            .map_err(|e| Error::Message {
                context: "Download error: ",
                detail: format!("Failed to download {}: {e}", url_owned),
            })?;
        let mut progress_reset = false;
        if resume_offset > 0 && response.status().as_u16() == 416 {
            match compio::fs::remove_file(&part_path_for_write).await {
                Ok(()) => {}
                Err(source) if source.kind() == ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::IoAt {
                        action: "remove file or directory",
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
                .map_err(|e| Error::Message {
                    context: "Download error: ",
                    detail: format!("Failed to attach User-Agent header: {e}"),
                })?;
            response = compio::time::timeout(send_timeout, retry_request.send())
                .await?
                .map_err(|e| Error::Message {
                    context: "Download error: ",
                    detail: format!(
                        "Failed to restart download {} after HTTP 416: {e}",
                        url_owned
                    ),
                })?;
        }

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Message {
                context: "Download error: ",
                detail: format!("HTTP error {}", status),
            });
        }

        if let Some(parent) = part_path_for_write.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::IoAt {
                    action: "create directory",
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
        let out = open_options
            .open(&part_path_for_write)
            .await
            .map_err(|e| Error::IoAt {
                action: "open file",
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
        let start_offset = if resume_effective { resume_offset } else { 0 };
        let mut last_reported_bytes = start_offset;
        let mut last_reported_at = Instant::now();
        let progress_threshold = (progress_buffer_bytes as u64).max(1);
        let progress = &on_progress;
        let (out, total_written) = super::download_write::write_http_body(
            response.bytes_stream(),
            out,
            &part_path_for_write,
            &url_owned,
            start_offset,
            body_timeout,
            |chunk| md5::Digest::update(&mut hasher, chunk),
            |written| {
                if let Some(callback) = progress.as_ref() {
                    let byte_threshold_reached =
                        written.saturating_sub(last_reported_bytes) >= progress_threshold;
                    if byte_threshold_reached
                        || last_reported_at.elapsed() >= PROGRESS_EMIT_INTERVAL
                    {
                        callback(DownloadProgress::Advanced(written));
                        last_reported_bytes = written;
                        last_reported_at = Instant::now();
                    }
                }
            },
        )
        .await?;
        if let Some(callback) = on_progress.as_ref() {
            if total_written > last_reported_bytes {
                callback(DownloadProgress::Advanced(total_written));
            }
        }

        out.sync_data().await.map_err(|e| Error::IoAt {
            action: "write to file",
            path: part_path_for_write.clone(),
            source: e,
        })?;

        if let Some(expected) = expected_size {
            if total_written != expected {
                return Err(Error::Message {
                    context: "Download error: ",
                    detail: format!(
                        "Downloaded size mismatch for {}: expected {}, got {}",
                        url_owned, expected, total_written
                    ),
                });
            }
        }

        let actual_md5 = crate::to_hex(&md5::Digest::finalize(hasher));
        Ok::<(u64, String), Error>((total_written, actual_md5))
    }?;

    if !actual_md5.eq_ignore_ascii_case(expected_md5) {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!(
                "MD5 mismatch: expected {}, got {}",
                expected_md5, actual_md5
            ),
        });
    }

    let expectation = ArtifactExpectation::new(logical_path, expected_md5, expected_size);
    let digest = ArtifactDigest::new(written, actual_md5);
    super::fs_ops::commit_observed_artifact(
        &part_path,
        dest,
        &expectation,
        ArtifactSource::Download,
        &digest,
    )
}

/// Stream one payload through the production HTTP writer, verify its size and
/// MD5, then remove the committed file immediately.
///
/// This is intentionally a small, opt-in live-test primitive. It proves CDN
/// transfer and atomic commit behavior without pretending to be a retained
/// install tree: callers must use a dedicated temporary root and must perform
/// any install, verify, repair, or hardlink assertions separately.
pub async fn download_and_discard(
    user_agent: &str,
    url: &str,
    logical_path: &str,
    expected_md5: &str,
    expected_size: u64,
    work_root: &Path,
    progress_buffer_bytes: usize,
) -> Result<StreamingDownloadReport> {
    compio::fs::create_dir_all(work_root)
        .await
        .map_err(|source| Error::IoAt {
            action: "create directory",
            path: work_root.to_path_buf(),
            source,
        })?;

    let destination = work_root.join("payload.bin");
    let partial = super::fs_ops::make_partial_download_path(&destination)?;
    let started = Instant::now();
    let result = do_prepared_download(
        user_agent,
        url,
        &destination,
        logical_path,
        expected_md5,
        Some(expected_size),
        DownloadResumeState::new(0, Md5::new()),
        progress_buffer_bytes,
        None::<fn(DownloadProgress)>,
    )
    .await;

    // A soak run must not accumulate either committed payloads or partial
    // downloads, including after a failed checksum or connection.
    let _ = compio::fs::remove_file(&destination).await;
    let _ = compio::fs::remove_file(&partial).await;

    let proof = result?;
    Ok(StreamingDownloadReport {
        bytes: proof.observed_size(),
        elapsed: started.elapsed(),
    })
}
