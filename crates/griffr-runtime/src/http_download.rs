use std::io::ErrorKind;
use std::path::Path;

use compio::buf::BufResult;
use compio::io::AsyncReadAt;
use md5::{Digest, Md5};

use crate::error::{Error, Result};

const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const USER_AGENT_HEADER: &str = "User-Agent";
const RANGE_HEADER: &str = "Range";

async fn hash_file_prefix(path: &Path, len: u64, hasher: &mut Md5) -> Result<()> {
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
                context: "Download error: ",
                detail: format!(
                    "resume prefix ended at byte {offset}, expected {len}: {}",
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

/// Stream an arbitrary HTTP resource to disk while computing its MD5.
///
/// This belongs to the runtime rather than a provider API crate because resume,
/// filesystem, timeout, and write-buffer policy are execution concerns.
pub async fn download_file(
    user_agent: &str,
    url: &str,
    output_path: &Path,
    resume: bool,
) -> Result<String> {
    let existing_len = if resume {
        match compio::fs::metadata(output_path).await {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => {
                return Err(Error::Message {
                    context: "Download error: ",
                    detail: format!(
                        "download destination is not a file: {}",
                        output_path.display()
                    ),
                });
            }
            Err(err) if err.kind() == ErrorKind::NotFound => 0,
            Err(source) => {
                return Err(Error::IoAt {
                    action: "query file metadata/stat for",
                    path: output_path.to_path_buf(),
                    source,
                });
            }
        }
    } else {
        0
    };

    thread_local! {
        static CLIENT: cyper::Client = cyper::Client::new().expect("failed to create thread-local HTTP client");
    }
    let client = CLIENT.with(Clone::clone);
    let mut request = client
        .get(url)?
        .header(USER_AGENT_HEADER, user_agent)
        .map_err(|source| Error::Message {
            context: "Download error: ",
            detail: format!("failed to attach User-Agent header: {source}"),
        })?;
    if existing_len > 0 {
        request = request
            .header(RANGE_HEADER, format!("bytes={existing_len}-"))
            .map_err(|source| Error::Message {
                context: "Download error: ",
                detail: format!("failed to set Range header: {source}"),
            })?;
    }

    let (send_timeout, body_timeout) = crate::task_pool::download::download_timeouts();
    let response = compio::time::timeout(send_timeout, request.send())
        .await?
        .map_err(|source| Error::Message {
            context: "Download error: ",
            detail: format!("failed to download {url}: {source}"),
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!("download returned HTTP {status}: {url}"),
        });
    }

    if let Some(parent) = output_path.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::IoAt {
                action: "create directory",
                path: parent.to_path_buf(),
                source,
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

    let mut options = compio::fs::OpenOptions::new();
    options.create(true).write(true).truncate(!resume_effective);
    let output = options
        .open(output_path)
        .await
        .map_err(|source| Error::IoAt {
            action: "open file",
            path: output_path.to_path_buf(),
            source,
        })?;
    let (output, _) = crate::task_pool::download_write::write_http_body(
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
    output.sync_data().await.map_err(|source| Error::IoAt {
        action: "write to file",
        path: output_path.to_path_buf(),
        source,
    })?;
    Ok(griffr_core::to_hex(&hasher.finalize()))
}

pub async fn download_file_with_verify(
    user_agent: &str,
    url: &str,
    output_path: &Path,
    expected_md5: &str,
) -> Result<()> {
    let actual = download_file(user_agent, url, output_path, false).await?;
    if !actual.eq_ignore_ascii_case(expected_md5) {
        return Err(Error::Message {
            context: "Download error: ",
            detail: format!("MD5 mismatch for {url}: expected {expected_md5}, got {actual}"),
        });
    }
    Ok(())
}
