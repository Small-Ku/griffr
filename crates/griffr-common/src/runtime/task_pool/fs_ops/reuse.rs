use std::fs::File;
use std::future::Future;
use std::io::ErrorKind;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::{Error, Result};
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
    dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }
        if compio::fs::metadata(&dest_owned).await.is_ok() {
            let _ = compio::fs::remove_file(&dest_owned).await;
        }
        compio::fs::hard_link(&src_owned, &dest_owned)
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "Failed to hardlink {} -> {}: {}",
                    src_owned.display(),
                    dest_owned.display(),
                    e
                ))
            })
    })?;
    Ok(())
}

pub(crate) enum ReuseMethod {
    Hardlink,
    Copy,
}

pub(crate) fn reuse_file(
    io_dispatcher: Option<&Dispatcher>,
    src: &Path,
    dest: &Path,
    allow_copy_fallback: bool,
) -> Result<ReuseMethod> {
    match create_hardlink(io_dispatcher, src, dest) {
        Ok(()) => Ok(ReuseMethod::Hardlink),
        Err(err) if allow_copy_fallback => {
            let dest_owned = dest.to_path_buf();
            dispatch_io(io_dispatcher, move || async move {
                if let Some(parent) = dest_owned.parent() {
                    compio::fs::create_dir_all(parent)
                        .await
                        .map_err(Error::from)?;
                }
                match compio::fs::metadata(&dest_owned).await {
                    Ok(_) => {
                        let _ = compio::fs::remove_file(&dest_owned).await;
                    }
                    Err(meta_err) if meta_err.kind() == ErrorKind::NotFound => {}
                    Err(meta_err) => return Err(meta_err.into()),
                }
                Ok::<(), Error>(())
            })?;
            std::fs::copy(src, dest).map_err(|e| Error::CopyFailed {
                src: src.to_path_buf(),
                dest: dest.to_path_buf(),
                source: e,
            })?;
            let dest_owned = dest.to_path_buf();
            let copied = dispatch_io(io_dispatcher, move || async move {
                compio::fs::metadata(&dest_owned)
                    .await
                    .map(|_| true)
                    .or_else(|meta_err| {
                        if meta_err.kind() == ErrorKind::NotFound {
                            Ok(false)
                        } else {
                            Err(meta_err)
                        }
                    })
                    .map_err(Error::from)
            })?;
            if !copied {
                return Err(err);
            }
            Ok(ReuseMethod::Copy)
        }
        Err(err) => Err(err),
    }
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
