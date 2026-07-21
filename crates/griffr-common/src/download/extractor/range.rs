use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use compio::buf::BufResult;
use compio::io::AsyncWriteAtExt;
use compio::time::timeout;
use futures_util::StreamExt;

use crate::api::protocol::{RANGE_HEADER, USER_AGENT_HEADER};
use crate::error::{Error, Result};

const ARCHIVE_RANGE_SEND_TIMEOUT: Duration = Duration::from_secs(60);
const ARCHIVE_RANGE_BODY_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// One exact HTTP range required by the archive planner. The range is local to
/// one package volume; `global_range` is the same data in the concatenated ZIP
/// address space.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct ArchiveRangeRequest {
    pub(crate) volume_index: usize,
    pub(crate) local_range: Range<u64>,
    pub(crate) global_range: Range<u64>,
    pub(crate) url: String,
    pub(crate) cache_path: PathBuf,
}

pub(crate) async fn fetch_archive_range_to_cache(
    request: &ArchiveRangeRequest,
    user_agent: &str,
    progress_buffer_bytes: usize,
    mut on_progress: impl FnMut(u64),
) -> Result<u64> {
    let expected = request.local_range.end - request.local_range.start;
    if expected == 0 {
        return Err(Error::Message {
            context: "Download error: ",
            detail: "Cannot download an empty archive byte range".to_string(),
        });
    }
    if let Some(parent) = request.cache_path.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::IoAt {
                action: "create directory",
                path: parent.to_path_buf(),
                source,
            })?;
    }

    let part_path = request.cache_path.with_extension("range.part");
    let mut resume_offset = compio::fs::metadata(&part_path)
        .await
        .ok()
        .map_or(0, |metadata| metadata.len());
    if resume_offset > expected {
        compio::fs::remove_file(&part_path)
            .await
            .map_err(|source| Error::IoAt {
                action: "remove file or directory",
                path: part_path.clone(),
                source,
            })?;
        resume_offset = 0;
    }
    if resume_offset == expected {
        save_archive_range_file(&part_path, &request.cache_path).await?;
        on_progress(expected);
        return Ok(expected);
    }
    if resume_offset > 0 {
        on_progress(resume_offset);
    }

    thread_local! {
        static CLIENT: cyper::Client = cyper::Client::new().expect("Failed to build HTTP client");
    }

    let fetch_start = request.local_range.start + resume_offset;
    let range_header = format!("bytes={}-{}", fetch_start, request.local_range.end - 1);
    let request_builder = CLIENT.with(|client| {
        client
            .get(&request.url)
            .map_err(|source| Error::Message {
                context: "Download error: ",
                detail: format!("HTTP error for {}: {source}", request.url),
            })?
            .header(USER_AGENT_HEADER, user_agent)
            .map_err(|source| Error::Message {
                context: "Download error: ",
                detail: format!("HTTP header error for {}: {source}", request.url),
            })?
            .header(RANGE_HEADER, range_header)
            .map_err(|source| Error::Message {
                context: "Download error: ",
                detail: format!("HTTP header error for {}: {source}", request.url),
            })
    })?;

    let response = timeout(ARCHIVE_RANGE_SEND_TIMEOUT, request_builder.send())
        .await
        .map_err(|_| Error::Message {
            context: "Download error: ",
            detail: format!("Timeout requesting archive range for {}", request.url),
        })?
        .map_err(|source| Error::Message {
            context: "Download error: ",
            detail: format!("HTTP download error for {}: {source}", request.url),
        })?;

    let status = response.status();
    if status.as_u16() != 206 {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!(
                "Server returned {status} for byte range request on {}",
                request.url
            ),
        });
    }

    let mut file = compio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part_path)
        .await
        .map_err(|source| Error::IoAt {
            action: "open file",
            path: part_path.clone(),
            source,
        })?;

    let mut current_offset = resume_offset;
    let mut buffered_bytes = 0usize;
    let flush_threshold = progress_buffer_bytes.max(64 * 1024);
    let mut stream = response.bytes_stream();

    loop {
        let chunk = timeout(ARCHIVE_RANGE_BODY_TIMEOUT, stream.next())
            .await
            .map_err(|_| Error::Message {
                context: "Download error: ",
                detail: format!("Timeout reading archive range body for {}", request.url),
            })?;

        let Some(chunk_res) = chunk else {
            break;
        };

        let chunk = chunk_res.map_err(|source| Error::Message {
            context: "Download error: ",
            detail: format!("HTTP download error for {}: {source}", request.url),
        })?;

        let chunk_len = chunk.len() as u64;
        let BufResult(result, _) = file.write_all_at(chunk, current_offset).await;
        result.map_err(|source| Error::IoAt {
            action: "write to file",
            path: part_path.clone(),
            source,
        })?;

        current_offset += chunk_len;
        buffered_bytes = buffered_bytes.saturating_add(chunk_len as usize);
        if buffered_bytes >= flush_threshold || current_offset == expected {
            on_progress(current_offset);
            buffered_bytes = 0;
        }
    }

    if current_offset != expected {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!(
                "Archive range payload size mismatch for {}: expected {} bytes, received {}",
                request.url, expected, current_offset
            ),
        });
    }

    file.close().await.map_err(|source| Error::IoAt {
        action: "write to file",
        path: part_path.clone(),
        source,
    })?;

    save_archive_range_file(&part_path, &request.cache_path).await?;
    Ok(expected)
}

async fn save_archive_range_file(part_path: &Path, cache_path: &Path) -> Result<()> {
    match compio::fs::metadata(cache_path).await {
        Ok(_) => {
            let _ = compio::fs::remove_file(part_path).await;
            return Ok(());
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
                path: cache_path.to_path_buf(),
                source,
            })
        }
    }

    if let Err(error) = compio::fs::rename(part_path, cache_path).await {
        if error.kind() == std::io::ErrorKind::AlreadyExists
            || compio::fs::metadata(cache_path).await.is_ok()
        {
            let _ = compio::fs::remove_file(part_path).await;
            return Ok(());
        }
        return Err(Error::IoBetween {
            action: "rename file",
            src: part_path.to_path_buf(),
            dest: cache_path.to_path_buf(),
            source: error,
        });
    }
    Ok(())
}
