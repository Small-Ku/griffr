use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
use compio::dispatcher::Dispatcher;
use md5::Md5;

pub(crate) fn dispatch_io<F, Fut, T>(io_dispatcher: Option<&Dispatcher>, task: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + 'static,
    T: Send + 'static,
{
    let dispatcher =
        io_dispatcher.ok_or_else(|| Error::TaskPool("IO dispatcher not available".to_string()))?;
    let mut receiver = dispatcher
        .dispatch(task)
        .map_err(|_| Error::TaskPool("Failed to dispatch IO task".to_string()))?;

    loop {
        match receiver.try_recv() {
            Ok(Some(result)) => return result,
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(_) => return Err(Error::TaskPool("IO task cancelled".to_string())),
        }
    }
}

pub(crate) fn make_partial_download_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        Error::InvalidPath(format!(
            "Destination path has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            Error::InvalidPath(format!(
                "Destination path has no file name: {}",
                path.display()
            ))
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
    let mut file = File::open(path).map_err(|e| Error::OpenFileFailed {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut remaining = prefix_len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            return Err(Error::Extraction(format!(
                "Partial file shorter than expected prefix: {} < {} for {}",
                prefix_len - remaining,
                prefix_len,
                path.display()
            )));
        }
        md5::Digest::update(hasher, &buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

pub(crate) fn commit_partial_download(
    io_dispatcher: Option<&Dispatcher>,
    part_path: &Path,
    dest_path: &Path,
) -> Result<()> {
    let part_owned = part_path.to_path_buf();
    let dest_owned = dest_path.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        match compio::fs::metadata(&part_owned).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(Error::Download(format!(
                    "Missing partial download file {}",
                    part_owned.display()
                )))
            }
            Err(err) => {
                return Err(Error::StatFailed {
                    path: part_owned,
                    source: err,
                })
            }
        }
        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        }
        match compio::fs::metadata(&dest_owned).await {
            Ok(_) => {
                compio::fs::remove_file(&dest_owned)
                    .await
                    .map_err(|e| Error::RemoveFailed {
                        path: dest_owned.clone(),
                        source: e,
                    })?
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(Error::StatFailed {
                    path: dest_owned,
                    source: err,
                })
            }
        }
        compio::fs::rename(&part_owned, &dest_owned)
            .await
            .map_err(|e| Error::RenameFailed {
                src: part_owned,
                dest: dest_owned,
                source: e,
            })?;
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn create_hardlink(
    io_dispatcher: Option<&Dispatcher>,
    src: &Path,
    dest: &Path,
) -> Result<()> {
    let src_owned = src.to_path_buf();
    let dest_owned = dest.to_path_buf();
    let temp_path = make_temp_write_path(dest)?;
    let temp_for_link = temp_path.clone();
    let link_result = dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }
        match compio::fs::metadata(&temp_for_link).await {
            Ok(_) => {
                compio::fs::remove_file(&temp_for_link)
                    .await
                    .map_err(|source| Error::RemoveFailed {
                        path: temp_for_link.clone(),
                        source,
                    })?;
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::StatFailed {
                    path: temp_for_link.clone(),
                    source,
                });
            }
        }
        compio::fs::hard_link(&src_owned, &temp_for_link)
            .await
            .map_err(|source| {
                Error::Other(format!(
                    "Failed to hardlink {} -> {}: {}",
                    src_owned.display(),
                    temp_for_link.display(),
                    source
                ))
            })
    });
    if let Err(error) = link_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = super::extract::move_path_replace(&temp_path, dest) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReuseMethod {
    Hardlink,
    Copy,
}

/// Reuses a source whose size and MD5 were already established by a CPU task.
/// Hardlinks therefore need no second read; copy fallback verifies inline.
pub(crate) fn reuse_verified_file(
    io_dispatcher: Option<&Dispatcher>,
    src: &Path,
    dest: &Path,
    expected_md5: &str,
    expected_size: u64,
    reuse_mode: ReuseMode,
    allow_copy_fallback: bool,
) -> Result<ReuseMethod> {
    if reuse_mode == ReuseMode::CopyOnly {
        if !allow_copy_fallback {
            return Err(Error::Other(format!(
                "Cannot reuse {} for {} across storage volumes because copy fallback is disabled",
                src.display(),
                dest.display()
            )));
        }
        copy_file_with_md5(src, dest, expected_md5, expected_size)?;
        return Ok(ReuseMethod::Copy);
    }

    match create_hardlink(io_dispatcher, src, dest) {
        Ok(()) => Ok(ReuseMethod::Hardlink),
        Err(_hardlink_error) if allow_copy_fallback => {
            copy_file_with_md5(src, dest, expected_md5, expected_size)?;
            Ok(ReuseMethod::Copy)
        }
        Err(error) => Err(error),
    }
}

fn copy_file_with_md5(
    src: &Path,
    dest: &Path,
    expected_md5: &str,
    expected_size: u64,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let temp = make_temp_write_path(dest)?;
    let _ = std::fs::remove_file(&temp);
    let mut input = File::open(src).map_err(|source| Error::OpenFileFailed {
        path: src.to_path_buf(),
        source,
    })?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|source| Error::WriteFileFailed {
            path: temp.clone(),
            source,
        })?;
    let mut hasher = <Md5 as md5::Digest>::new();
    let mut copied = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    let copy_result = (|| -> Result<()> {
        preallocate_file(&output, &temp, expected_size)?;
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
            md5::Digest::update(&mut hasher, &buffer[..read]);
            copied = copied.saturating_add(read as u64);
        }
        output.sync_all().map_err(|source| Error::WriteFileFailed {
            path: temp.clone(),
            source,
        })?;
        let actual_md5 = format!("{:x}", md5::Digest::finalize(hasher));
        if copied != expected_size || actual_md5 != expected_md5.to_lowercase() {
            return Err(Error::Integrity(format!(
                "Copy verification failed for {} -> {}: expected size/md5 {}/{}, got {}/{}",
                src.display(),
                dest.display(),
                expected_size,
                expected_md5,
                copied,
                actual_md5
            )));
        }
        if let Ok(metadata) = std::fs::metadata(src) {
            let _ = std::fs::set_permissions(&temp, metadata.permissions());
        }
        drop(output);
        super::extract::move_path_replace(&temp, dest)?;
        Ok(())
    })();
    if copy_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    copy_result
}

pub(crate) fn make_temp_write_path(path: &Path) -> Result<PathBuf> {
    static TEMP_WRITE_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let parent = path.parent().ok_or_else(|| {
        Error::InvalidPath(format!(
            "Destination path has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            Error::InvalidPath(format!(
                "Destination path has no file name: {}",
                path.display()
            ))
        })?
        .to_string_lossy();
    let counter = TEMP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{}.griffr.tmp.{}", file_name, counter)))
}

#[cfg(test)]
pub(crate) fn write_file(
    io_dispatcher: Option<&Dispatcher>,
    path: &Path,
    bytes: Vec<u8>,
) -> Result<()> {
    let path_owned = path.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = path_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }
        let temp_path = make_temp_write_path(&path_owned)?;
        let write_res = compio::fs::write(&temp_path, bytes).await;
        if let Err(err) = write_res.0 {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(Error::WriteFileFailed {
                path: temp_path,
                source: err,
            });
        }
        match compio::fs::metadata(&path_owned).await {
            Ok(_) => {
                compio::fs::remove_file(&path_owned)
                    .await
                    .map_err(|e| Error::RemoveFailed {
                        path: path_owned.clone(),
                        source: e,
                    })?
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                let _ = compio::fs::remove_file(&temp_path).await;
                return Err(Error::StatFailed {
                    path: path_owned,
                    source: err,
                });
            }
        }
        if let Err(err) = compio::fs::rename(&temp_path, &path_owned).await {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(Error::RenameFailed {
                src: temp_path,
                dest: path_owned,
                source: err,
            });
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        classify_reuse_mode, copy_file_with_md5, reuse_verified_file, storage_volume_id,
        ReuseMethod, ReuseMode,
    };
    use compio::dispatcher::Dispatcher;
    use md5::Md5;
    use std::num::NonZeroUsize;

    #[test]
    fn volume_classification_only_forces_copy_for_proven_differences() {
        assert_eq!(
            classify_reuse_mode(Some("volume-a"), Some("volume-a")),
            ReuseMode::HardlinkPreferred
        );
        assert_eq!(
            classify_reuse_mode(Some("volume-a"), Some("volume-b")),
            ReuseMode::CopyOnly
        );
        assert_eq!(
            classify_reuse_mode(None, Some("volume-b")),
            ReuseMode::HardlinkPreferred
        );
    }

    #[test]
    fn hardlink_reuses_the_already_verified_inode_without_rehashing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        std::fs::write(&source, b"verified-before-reuse").unwrap();
        let dispatcher = Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(2).unwrap())
            .build()
            .unwrap();

        let method = reuse_verified_file(
            Some(&dispatcher),
            &source,
            &destination,
            "00000000000000000000000000000000",
            0,
            ReuseMode::HardlinkPreferred,
            false,
        )
        .unwrap();

        assert!(matches!(method, ReuseMethod::Hardlink));
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"verified-before-reuse"
        );
    }

    #[test]
    fn failed_hardlink_keeps_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("missing-source.bin");
        let destination = temp.path().join("destination.bin");
        std::fs::write(&destination, b"keep-me").unwrap();
        let dispatcher = Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(2).unwrap())
            .build()
            .unwrap();

        reuse_verified_file(
            Some(&dispatcher),
            &source,
            &destination,
            "00000000000000000000000000000000",
            0,
            ReuseMode::HardlinkPreferred,
            false,
        )
        .unwrap_err();

        assert_eq!(std::fs::read(&destination).unwrap(), b"keep-me");
    }

    #[test]
    fn copy_hashes_while_writing_and_commits_verified_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let payload = b"copy-and-hash-in-one-pass";
        std::fs::write(&source, payload).unwrap();
        std::fs::write(&destination, b"old").unwrap();
        let expected_md5 = format!("{:x}", <Md5 as md5::Digest>::digest(payload));

        copy_file_with_md5(&source, &destination, &expected_md5, payload.len() as u64).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), payload);
    }

    #[test]
    fn copy_mismatch_keeps_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        std::fs::write(&source, b"new-data").unwrap();
        std::fs::write(&destination, b"old-data").unwrap();

        let error =
            copy_file_with_md5(&source, &destination, "00000000000000000000000000000000", 8)
                .unwrap_err();

        assert!(error.to_string().contains("Copy verification failed"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"old-data");
    }

    #[test]
    fn copy_only_reuse_skips_hardlink_and_verifies_copy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let payload = b"copy-only-source";
        std::fs::write(&source, payload).unwrap();
        let expected_md5 = format!("{:x}", <Md5 as md5::Digest>::digest(payload));
        let dispatcher = Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(2).unwrap())
            .build()
            .unwrap();

        let method = reuse_verified_file(
            Some(&dispatcher),
            &source,
            &destination,
            &expected_md5,
            payload.len() as u64,
            ReuseMode::CopyOnly,
            true,
        )
        .unwrap();

        assert_eq!(method, ReuseMethod::Copy);
        assert_eq!(std::fs::read(&destination).unwrap(), payload);
    }

    #[test]
    fn volume_identity_is_stable_within_one_temp_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("nested").join("destination.bin");
        std::fs::write(&source, b"source").unwrap();

        assert_eq!(
            storage_volume_id(&source),
            storage_volume_id(&destination),
            "missing destination paths should resolve through their existing ancestor"
        );
    }
}
