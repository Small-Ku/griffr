use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use compio::buf::BufResult;
use compio::io::{AsyncReadAt, AsyncWriteAtExt};

use crate::error::{Error, Result};
use crate::runtime::{preallocate_file, PathReuseMethod};
use md5::Md5;

pub(crate) fn make_partial_download_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| Error::Message {
        context: "Invalid path: ",
        detail: format!("Destination path has no parent: {}", path.display()),
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::Message {
            context: "Invalid path: ",
            detail: format!("Destination path has no file name: {}", path.display()),
        })?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.griffr.part", file_name)))
}

pub(crate) fn hash_file_prefix_into_hasher(
    path: &Path,
    prefix_len: u64,
    hasher: &mut Md5,
) -> Result<()> {
    if prefix_len == 0 {
        return Ok(());
    }
    let mut file = File::open(path).map_err(|e| Error::IoAt {
        action: "open file",
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut remaining = prefix_len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Partial file shorter than expected prefix: {} < {} for {}",
                    prefix_len - remaining,
                    prefix_len,
                    path.display()
                ),
            });
        }
        md5::Digest::update(hasher, &buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

pub(crate) fn commit_partial_download(part_path: &Path, dest_path: &Path) -> Result<()> {
    match std::fs::metadata(part_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::Message {
                context: "Download error: ",
                detail: format!(
                    "Partial download path is not a file: {}",
                    part_path.display()
                ),
            })
        }
        Err(source) if source.kind() == ErrorKind::NotFound => {
            return Err(Error::Message {
                context: "Download error: ",
                detail: format!("Missing partial download file {}", part_path.display()),
            })
        }
        Err(source) => {
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
                path: part_path.to_path_buf(),
                source,
            })
        }
    }
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    super::extract::move_path_replace(part_path, dest_path)
}

pub(crate) async fn commit_partial_download_async(
    part_path: &Path,
    dest_path: &Path,
) -> Result<()> {
    match compio::fs::metadata(part_path).await {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::Message {
                context: "Download error: ",
                detail: format!(
                    "Partial download path is not a file: {}",
                    part_path.display()
                ),
            })
        }
        Err(source) if source.kind() == ErrorKind::NotFound => {
            return Err(Error::Message {
                context: "Download error: ",
                detail: format!("Missing partial download file {}", part_path.display()),
            })
        }
        Err(source) => {
            return Err(Error::IoAt {
                action: "query file metadata/stat for",
                path: part_path.to_path_buf(),
                source,
            })
        }
    }
    if let Some(parent) = dest_path.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::IoAt {
                action: "create directory",
                path: parent.to_path_buf(),
                source,
            })?;
    }
    compio::fs::rename(part_path, dest_path)
        .await
        .map_err(|source| Error::IoBetween {
            action: "rename file",
            src: part_path.to_path_buf(),
            dest: dest_path.to_path_buf(),
            source,
        })
}

pub(crate) async fn create_hardlink_async(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::IoAt {
                action: "create directory",
                path: parent.to_path_buf(),
                source,
            })?;
    }
    let temp_path = make_temp_write_path(dest)?;
    match compio::fs::remove_file(&temp_path).await {
        Ok(()) => {}
        Err(source) if source.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::IoAt {
                action: "remove file or directory",
                path: temp_path,
                source,
            })
        }
    }
    if let Err(source) = compio::fs::hard_link(src, &temp_path).await {
        let _ = compio::fs::remove_file(&temp_path).await;
        return Err(Error::Message {
            context: "",
            detail: format!(
                "Failed to hardlink {} -> {}: {}",
                src.display(),
                temp_path.display(),
                source
            ),
        });
    }
    if let Err(source) = compio::fs::rename(&temp_path, dest).await {
        let _ = compio::fs::remove_file(&temp_path).await;
        return Err(Error::IoBetween {
            action: "rename file",
            src: temp_path,
            dest: dest.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn existing_volume_probe(path: &Path) -> Option<PathBuf> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    std::fs::canonicalize(probe)
        .ok()
        .or_else(|| Some(probe.to_path_buf()))
}

#[cfg(windows)]
pub(crate) fn storage_volume_id(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

    const BUFFER_LEN: usize = 32_768;
    let probe = existing_volume_probe(path)?;
    let mut wide = probe.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let mut mount_path = vec![0u16; BUFFER_LEN];
    if unsafe {
        GetVolumePathNameW(
            wide.as_ptr(),
            mount_path.as_mut_ptr(),
            mount_path.len() as u32,
        )
    } == 0
    {
        return None;
    }
    let mount_len = mount_path
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(mount_path.len());

    let mut volume_name = vec![0u16; BUFFER_LEN];
    let identity = if unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_path.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    } != 0
    {
        let len = volume_name
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(volume_name.len());
        String::from_utf16_lossy(&volume_name[..len])
    } else {
        String::from_utf16_lossy(&mount_path[..mount_len])
    };
    Some(identity.to_ascii_lowercase())
}

#[cfg(unix)]
pub(crate) fn storage_volume_id(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let probe = existing_volume_probe(path)?;
    let metadata = std::fs::metadata(probe).ok()?;
    Some(format!("device:{}", metadata.dev()))
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn storage_volume_id(path: &Path) -> Option<String> {
    existing_volume_probe(path).and_then(|probe| {
        probe
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
    })
}

pub(crate) fn storage_volume_group_key(path: &Path) -> String {
    storage_volume_id(path).unwrap_or_else(|| {
        path.components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReuseMode {
    HardlinkPreferred,
    CopyOnly,
}

/// Classifies only identities proven to differ as copy-only. Unknown identities
/// preserve the hardlink-first path so a temporary volume-query failure cannot
/// disable otherwise valid reuse.
pub(crate) fn classify_reuse_mode(
    source_volume: Option<&str>,
    destination_volume: Option<&str>,
) -> ReuseMode {
    match (source_volume, destination_volume) {
        (Some(source), Some(destination)) if source != destination => ReuseMode::CopyOnly,
        _ => ReuseMode::HardlinkPreferred,
    }
}

pub(crate) async fn copy_verified_file_async(
    src: &Path,
    dest: &Path,
    expected_md5: &str,
    expected_size: u64,
) -> Result<PathReuseMethod> {
    const COPY_BUFFER_BYTES: usize = 1024 * 1024;

    if let Some(parent) = dest.parent() {
        compio::fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::IoAt {
                action: "create directory",
                path: parent.to_path_buf(),
                source,
            })?;
    }

    let temp = make_temp_write_path(dest)?;
    match compio::fs::remove_file(&temp).await {
        Ok(()) => {}
        Err(source) if source.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::IoAt {
                action: "remove file or directory",
                path: temp,
                source,
            })
        }
    }

    let source_permissions = compio::fs::metadata(src)
        .await
        .ok()
        .map(|metadata| metadata.permissions());
    let input = compio::fs::File::open(src)
        .await
        .map_err(|source| Error::IoAt {
            action: "open file",
            path: src.to_path_buf(),
            source,
        })?;
    let mut output = compio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .await
        .map_err(|source| Error::IoAt {
            action: "write to file",
            path: temp.clone(),
            source,
        })?;

    let copy_result = async {
        preallocate_file(&output, &temp, expected_size)?;
        let mut hasher = <Md5 as md5::Digest>::new();
        let mut copied = 0u64;
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        loop {
            let BufResult(read_result, mut returned_buffer) = input.read_at(buffer, copied).await;
            let read = read_result.map_err(|source| Error::IoBetween {
                action: "copy file",
                src: src.to_path_buf(),
                dest: dest.to_path_buf(),
                source,
            })?;
            if read == 0 {
                break;
            }
            returned_buffer.truncate(read);
            md5::Digest::update(&mut hasher, &returned_buffer);
            let BufResult(write_result, mut returned_buffer) =
                output.write_all_at(returned_buffer, copied).await;
            write_result.map_err(|source| Error::IoBetween {
                action: "copy file",
                src: src.to_path_buf(),
                dest: dest.to_path_buf(),
                source,
            })?;
            copied = copied.saturating_add(read as u64);
            returned_buffer.resize(COPY_BUFFER_BYTES, 0);
            buffer = returned_buffer;
        }

        output.sync_all().await.map_err(|source| Error::IoAt {
            action: "write to file",
            path: temp.clone(),
            source,
        })?;
        let actual_md5 = crate::to_hex(&md5::Digest::finalize(hasher));
        if copied != expected_size || actual_md5 != expected_md5.to_lowercase() {
            return Err(Error::Message {
                context: "Integrity error: ",
                detail: format!(
                    "Copy verification failed for {} -> {}: expected size/md5 {}/{}, got {}/{}",
                    src.display(),
                    dest.display(),
                    expected_size,
                    expected_md5,
                    copied,
                    actual_md5
                ),
            });
        }
        if let Some(permissions) = source_permissions {
            // Preserve the previous reuse behavior: permissions are best-effort
            // and must not invalidate an otherwise verified byte-for-byte copy.
            let _ = compio::fs::set_permissions(&temp, permissions).await;
        }
        Ok(())
    }
    .await;

    // Explicitly close on both success and failure. Dropping a compio file may
    // defer the actual OS close, which can otherwise keep the temporary path
    // busy while cleanup runs on Windows.
    let close_result = output.close().await.map_err(|source| Error::IoAt {
        action: "write to file",
        path: temp.clone(),
        source,
    });
    if let Err(error) = copy_result {
        let _ = close_result;
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(error) = close_result {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(source) = compio::fs::rename(&temp, dest).await {
        let _ = compio::fs::remove_file(&temp).await;
        return Err(Error::IoBetween {
            action: "rename file",
            src: temp,
            dest: dest.to_path_buf(),
            source,
        });
    }
    Ok(PathReuseMethod::Copy)
}

pub(crate) fn make_temp_write_path(path: &Path) -> Result<PathBuf> {
    static TEMP_WRITE_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let parent = path.parent().ok_or_else(|| Error::Message {
        context: "Invalid path: ",
        detail: format!("Destination path has no parent: {}", path.display()),
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::Message {
            context: "Invalid path: ",
            detail: format!("Destination path has no file name: {}", path.display()),
        })?
        .to_string_lossy();
    let counter = TEMP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{}.griffr.tmp.{}", file_name, counter)))
}

#[cfg(test)]
pub(crate) fn write_file(path: &Path, bytes: Vec<u8>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp_path = make_temp_write_path(path)?;
    let result = (|| -> Result<()> {
        std::fs::write(&temp_path, bytes).map_err(|source| Error::IoAt {
            action: "write to file",
            path: temp_path.clone(),
            source,
        })?;
        super::extract::move_path_replace(&temp_path, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests;
