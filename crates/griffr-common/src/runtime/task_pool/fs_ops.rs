use std::fs::File;
use std::future::Future;
use std::io::ErrorKind;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use compio::dispatcher::Dispatcher;
use md5::Md5;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub(crate) fn dispatch_io<F, Fut, T>(io_dispatcher: Option<&Dispatcher>, task: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + 'static,
    T: Send + 'static,
{
    let dispatcher = io_dispatcher.context("IO dispatcher not available")?;
    let mut receiver = dispatcher
        .dispatch(task)
        .map_err(|_| anyhow::anyhow!("Failed to dispatch IO task"))?;

    loop {
        match receiver.try_recv() {
            Ok(Some(result)) => return result,
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(_) => anyhow::bail!("IO task cancelled"),
        }
    }
}

pub(crate) fn make_partial_download_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("Destination path has no parent")?;
    let file_name = path
        .file_name()
        .context("Destination path has no file name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.griffr.part", file_name)))
}

pub(crate) fn hash_file_prefix_into_hasher(path: &Path, prefix_len: u64, hasher: &mut Md5) -> Result<()> {
    if prefix_len == 0 {
        return Ok(());
    }
    let mut file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut remaining = prefix_len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            anyhow::bail!("Partial file shorter than expected prefix: {} < {} for {}", prefix_len - remaining, prefix_len, path.display());
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
            Err(err) if err.kind() == ErrorKind::NotFound => anyhow::bail!("Missing partial download file {}", part_owned.display()),
            Err(err) => return Err(err).with_context(|| format!("Failed to stat {}", part_owned.display())),
        }
        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent).await.with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        match compio::fs::metadata(&dest_owned).await {
            Ok(_) => compio::fs::remove_file(&dest_owned).await.with_context(|| format!("Failed to replace {}", dest_owned.display()))?,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("Failed to stat {}", dest_owned.display())),
        }
        compio::fs::rename(&part_owned, &dest_owned)
            .await
            .with_context(|| format!("Failed to move {} to {}", part_owned.display(), dest_owned.display()))?;
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn make_extract_staging_dir(dest: &Path, base_name: &str) -> Result<PathBuf> {
    static EXTRACT_STAGING_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let counter = EXTRACT_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let parent = dest.parent().unwrap_or(dest);
    Ok(parent.join(format!(".griffr.extract.{}.{}", base_name, counter)))
}

pub(crate) fn commit_staged_extract(staging_root: &Path, dest_root: &Path) -> Result<()> {
    commit_staged_extract_inner(staging_root, staging_root, dest_root)?;
    std::fs::remove_dir_all(staging_root)
        .with_context(|| format!("Failed to clean extraction staging directory {}", staging_root.display()))?;
    Ok(())
}

fn commit_staged_extract_inner(staging_root: &Path, current: &Path, dest_root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(current).with_context(|| format!("Failed to read directory {}", current.display()))? {
        let entry = entry.with_context(|| format!("Failed to read directory entry under {}", current.display()))?;
        let src_path = entry.path();
        let file_type = entry.file_type().with_context(|| format!("Failed to inspect directory entry {}", src_path.display()))?;
        let relative = src_path
            .strip_prefix(staging_root)
            .with_context(|| format!("Failed to derive relative path for staged entry {}", src_path.display()))?;
        let dest_path = dest_root.join(relative);
        if file_type.is_dir() {
            std::fs::create_dir_all(&dest_path).with_context(|| format!("Failed to create directory {}", dest_path.display()))?;
            commit_staged_extract_inner(staging_root, &src_path, dest_root)?;
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        if dest_path.exists() && dest_path.is_dir() {
            std::fs::remove_dir_all(&dest_path).with_context(|| format!("Failed to replace {}", dest_path.display()))?;
        }
        move_path_replace(&src_path, &dest_path)
            .with_context(|| format!("Failed to move extracted file {} -> {}", src_path.display(), dest_path.display()))?;
    }
    Ok(())
}

fn move_path_replace(src: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let mut src_wide: Vec<u16> = src.as_os_str().encode_wide().collect();
        src_wide.push(0);
        let mut dest_wide: Vec<u16> = dest.as_os_str().encode_wide().collect();
        dest_wide.push(0);
        let moved = unsafe {
            MoveFileExW(
                src_wide.as_ptr(),
                dest_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("MoveFileExW failed to replace {} -> {}", src.display(), dest.display()));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if dest.exists() {
            if dest.is_dir() {
                std::fs::remove_dir_all(dest).with_context(|| format!("Failed to replace {}", dest.display()))?;
            } else {
                std::fs::remove_file(dest).with_context(|| format!("Failed to replace {}", dest.display()))?;
            }
        }
        std::fs::rename(src, dest).with_context(|| format!("Failed to rename staged path {} -> {}", src.display(), dest.display()))?;
        Ok(())
    }
}

pub(crate) fn create_hardlink(io_dispatcher: Option<&Dispatcher>, src: &Path, dest: &Path) -> Result<()> {
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
            .with_context(|| format!("Failed to hardlink {} -> {}", src_owned.display(), dest_owned.display()))
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
                    compio::fs::create_dir_all(parent).await.map_err(anyhow::Error::from)?;
                }
                match compio::fs::metadata(&dest_owned).await {
                    Ok(_) => {
                        let _ = compio::fs::remove_file(&dest_owned).await;
                    }
                    Err(meta_err) if meta_err.kind() == ErrorKind::NotFound => {}
                    Err(meta_err) => return Err(meta_err.into()),
                }
                Ok::<(), anyhow::Error>(())
            })?;
            std::fs::copy(src, dest).with_context(|| format!("Failed to copy {} -> {}", src.display(), dest.display()))?;
            let dest_owned = dest.to_path_buf();
            let copied = dispatch_io(io_dispatcher, move || async move {
                compio::fs::metadata(&dest_owned)
                    .await
                    .map(|_| true)
                    .or_else(|meta_err| if meta_err.kind() == ErrorKind::NotFound { Ok(false) } else { Err(meta_err) })
                    .map_err(anyhow::Error::from)
            })?;
            if !copied {
                return Err(err);
            }
            Ok(ReuseMethod::Copy)
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
pub(crate) fn make_temp_write_path(path: &Path) -> Result<PathBuf> {
    static TEMP_WRITE_COUNTER: AtomicUsize = AtomicUsize::new(0);
    let parent = path.parent().context("Destination path has no parent")?;
    let file_name = path.file_name().context("Destination path has no file name")?.to_string_lossy();
    let counter = TEMP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{}.griffr.tmp.{}", file_name, counter)))
}

#[cfg(test)]
pub(crate) fn write_file(io_dispatcher: Option<&Dispatcher>, path: &Path, bytes: Vec<u8>) -> Result<()> {
    let path_owned = path.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = path_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }
        let temp_path = make_temp_write_path(&path_owned)?;
        let write_res = compio::fs::write(&temp_path, bytes).await;
        if let Err(err) = write_res.0 {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(err).with_context(|| format!("Failed to write temp file {}", temp_path.display()));
        }
        match compio::fs::metadata(&path_owned).await {
            Ok(_) => compio::fs::remove_file(&path_owned).await.with_context(|| format!("Failed to replace {}", path_owned.display()))?,
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                let _ = compio::fs::remove_file(&temp_path).await;
                return Err(err).with_context(|| format!("Failed to stat {}", path_owned.display()));
            }
        }
        if let Err(err) = compio::fs::rename(&temp_path, &path_owned).await {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(err).with_context(|| format!("Failed to move temp file to {}", path_owned.display()));
        }
        Ok(())
    })?;
    Ok(())
}
