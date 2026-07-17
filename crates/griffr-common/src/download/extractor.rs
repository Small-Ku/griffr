//! Multi-volume zip extraction

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use crate::api::types::ResourcePatch;
use crate::error::{Error, Result};
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME};

/// Cached byte range for one split archive volume.
#[derive(Debug, Clone)]
struct VolumeLayout {
    path: PathBuf,
    start: u64,
    end: u64,
}

/// Multi-volume stream that reads split zip files as a single stream.
///
/// Volume sizes and cumulative offsets are captured once at construction so
/// ZipArchive seeks do not restat every part or linearly rescan the full set.
pub struct MultiVolumeStream {
    layouts: Vec<VolumeLayout>,
    total_size: u64,
    current_volume: usize,
    current_file: Option<std::fs::File>,
    position: u64,
}

impl MultiVolumeStream {
    /// Create a new multi-volume stream from a list of volume paths.
    pub(crate) fn new(volumes: Vec<PathBuf>) -> Result<Self> {
        if volumes.is_empty() {
            return Err(Error::Extraction("No volumes provided".to_string()));
        }

        let mut start = 0u64;
        let mut layouts = Vec::with_capacity(volumes.len());
        for path in volumes {
            let metadata = std::fs::metadata(&path).map_err(|source| Error::StatFailed {
                path: path.clone(),
                source,
            })?;
            let end = start.checked_add(metadata.len()).ok_or_else(|| {
                Error::Extraction("Combined archive size overflowed u64".to_string())
            })?;
            layouts.push(VolumeLayout { path, start, end });
            start = end;
        }

        let mut stream = Self {
            layouts,
            total_size: start,
            current_volume: 0,
            current_file: None,
            position: 0,
        };
        stream.open_current_volume()?;
        Ok(stream)
    }

    fn open_current_volume(&mut self) -> Result<()> {
        let layout = self.layouts.get(self.current_volume).ok_or_else(|| {
            Error::Extraction("No more volumes to open".to_string())
        })?;
        let file = std::fs::File::open(&layout.path).map_err(|source| Error::OpenFileFailed {
            path: layout.path.clone(),
            source,
        })?;
        self.current_file = Some(file);
        Ok(())
    }

    fn next_volume(&mut self) -> Result<bool> {
        self.current_volume += 1;
        if self.current_volume < self.layouts.len() {
            self.open_current_volume()?;
            Ok(true)
        } else {
            self.current_file = None;
            Ok(false)
        }
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }
}

impl Read for MultiVolumeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match &mut self.current_file {
                Some(file) => {
                    let bytes_read = file.read(buf)?;
                    if bytes_read > 0 {
                        self.position = self.position.saturating_add(bytes_read as u64);
                        return Ok(bytes_read);
                    }
                    match self.next_volume() {
                        Ok(true) => continue,
                        Ok(false) => return Ok(0),
                        Err(error) => return Err(std::io::Error::other(error)),
                    }
                }
                None => return Ok(0),
            }
        }
    }
}

impl Seek for MultiVolumeStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target_position = match pos {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.total_size) + i128::from(offset),
        };
        if target_position < 0 || target_position > i128::from(self.total_size) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Invalid seek position: {} (total size: {})",
                    target_position, self.total_size
                ),
            ));
        }
        let target = target_position as u64;
        if target == self.total_size {
            let last_index = self.layouts.len() - 1;
            if self.current_volume != last_index {
                self.current_volume = last_index;
                self.open_current_volume().map_err(std::io::Error::other)?;
            }
            if let Some(file) = &mut self.current_file {
                file.seek(SeekFrom::End(0))?;
            }
            self.position = target;
            return Ok(target);
        }

        let index = self.layouts.partition_point(|layout| layout.end <= target);
        let layout = self.layouts.get(index).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Seek position beyond end of stream",
            )
        })?;
        let offset = target.saturating_sub(layout.start);
        if self.current_volume != index {
            self.current_volume = index;
            self.open_current_volume().map_err(std::io::Error::other)?;
        }
        if let Some(file) = &mut self.current_file {
            file.seek(SeekFrom::Start(offset))?;
        }
        self.position = target;
        Ok(target)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveInspection {
    pub entries: BTreeMap<String, u64>,
    pub total_uncompressed_bytes: u64,
    pub patch_manifest: Option<ResourcePatch>,
    pub delete_manifest: Option<String>,
}

fn open_archive_entry<'a, R: Read + Seek>(
    archive: &'a mut zip::ZipArchive<R>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a>> {
    match password {
        Some(password) => archive
            .by_index_decrypt(index, password.as_bytes())
            .map_err(|err| {
                Error::Extraction(format!(
                    "Failed to decrypt archive entry {index}; archive password may be missing or incorrect: {err}"
                ))
            }),
        None => archive.by_index(index).map_err(|err| {
            Error::Extraction(format!(
                "Failed to open archive entry {index}; if the archive is password-protected, provide its package key: {err}"
            ))
        }),
    }
}

fn safe_relative_archive_path(name: &str) -> Result<PathBuf> {
    let rel = Path::new(name);
    if rel.is_absolute() {
        return Err(Error::InvalidPath(format!(
            "Zip entry has absolute path: {name}"
        )));
    }
    for component in rel.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(Error::InvalidPath(format!(
                    "Zip entry has unsafe path: {name}"
                )))
            }
            _ => {}
        }
    }
    Ok(rel.to_path_buf())
}

fn normalized_archive_name(name: &str) -> Result<String> {
    Ok(safe_relative_archive_path(name)?
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string())
}

/// Multi-volume zip extractor
pub(crate) struct MultiVolumeExtractor {
    volumes: Vec<PathBuf>,
}

impl MultiVolumeExtractor {
    /// Create a new extractor from a list of volume paths
    pub(crate) fn new(volumes: Vec<PathBuf>) -> Result<Self> {
        if volumes.is_empty() {
            return Err(Error::Extraction("No volumes provided".to_string()));
        }

        Ok(Self { volumes })
    }

    pub(crate) fn inspect_patch_payload(
        &self,
        password: Option<&str>,
    ) -> Result<ArchiveInspection> {
        const MAX_CONTROL_FILE_BYTES: u64 = 16 * 1024 * 1024;

        let stream = MultiVolumeStream::new(self.volumes.clone())?;
        let mut archive = zip::ZipArchive::new(stream)?;
        let mut entries = BTreeMap::new();
        let mut total_uncompressed_bytes = 0u64;
        let mut patch_manifest = None;
        let mut delete_manifest = None;

        for index in 0..archive.len() {
            let mut file = open_archive_entry(&mut archive, index, password)?;
            let name = normalized_archive_name(file.name())?;
            if name.is_empty() || file.is_dir() {
                continue;
            }
            let size = file.size();
            total_uncompressed_bytes = total_uncompressed_bytes.saturating_add(size);
            if entries.insert(name.clone(), size).is_some() {
                return Err(Error::Extraction(format!(
                    "Archive contains duplicate entry {name}"
                )));
            }
            if name == PATCH_MANIFEST_NAME || name == DELETE_FILES_MANIFEST_NAME {
                if size > MAX_CONTROL_FILE_BYTES {
                    return Err(Error::Extraction(format!(
                        "Archive control file {name} is unexpectedly large ({size} bytes)"
                    )));
                }
                let mut payload = Vec::with_capacity(size as usize);
                file.read_to_end(&mut payload)?;
                if name == PATCH_MANIFEST_NAME {
                    patch_manifest = Some(serde_json::from_slice(&payload)?);
                } else {
                    delete_manifest = Some(String::from_utf8(payload).map_err(|err| {
                        Error::Extraction(format!(
                            "{DELETE_FILES_MANIFEST_NAME} is not UTF-8: {err}"
                        ))
                    })?);
                }
            }
        }

        Ok(ArchiveInspection {
            entries,
            total_uncompressed_bytes,
            patch_manifest,
            delete_manifest,
        })
    }

    pub(crate) fn extract_to_with_progress(
        &self,
        target_dir: &Path,
        password: Option<&str>,
        total_extract_bytes: u64,
        progress_buffer_bytes: usize,
        mut progress_callback: Option<impl FnMut(u64, u64)>,
    ) -> Result<()> {
        let stream = MultiVolumeStream::new(self.volumes.clone())?;
        let mut archive = zip::ZipArchive::new(stream)?;
        let mut extracted_bytes = 0u64;
        if let Some(ref mut cb) = progress_callback {
            cb(0, total_extract_bytes);
        }

        // Extract all files
        for i in 0..archive.len() {
            let mut file = open_archive_entry(&mut archive, i, password)?;
            let file_path = target_dir.join(safe_relative_archive_path(file.name())?);

            if file.is_dir() {
                std::fs::create_dir_all(&file_path).map_err(|e| Error::CreateDirFailed {
                    path: file_path.clone(),
                    source: e,
                })?;
                continue;
            }

            // Create parent directory if needed
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }

            // Extract file (overwrite if exists)
            let mut output =
                std::fs::File::create(&file_path).map_err(|e| Error::OpenFileFailed {
                    path: file_path.clone(),
                    source: e,
                })?;
            let buffer_size = progress_buffer_bytes.max(4 * 1024);
            let mut buf = vec![0u8; buffer_size];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &buf[..n])?;
                extracted_bytes = extracted_bytes.saturating_add(n as u64);
                if let Some(ref mut cb) = progress_callback {
                    cb(extracted_bytes, total_extract_bytes);
                }
            }
        }

        Ok(())
    }

    /// Delete volumes after successful extraction
    pub fn cleanup(&self) -> Result<()> {
        for volume in &self.volumes {
            if let Err(e) = std::fs::remove_file(volume) {
                tracing::warn!("Failed to delete volume {}: {}", volume.display(), e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_multi_volume_extractor() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let base_path = temp_dir.path();

        // 1. Create a zip archive and split it
        let zip_path = base_path.join("test.zip");
        let file = std::fs::File::create(&zip_path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("hello.txt", zip::write::FileOptions::<()>::default())?;
        zip.write_all(b"Hello, World!")?;
        zip.finish()?;

        let data = std::fs::read(&zip_path)?;
        let chunk_size = 5;
        let mut volumes = Vec::new();
        for (i, chunk) in data.chunks(chunk_size).enumerate() {
            let volume_path = base_path.join(format!("test.zip.{:03}", i + 1));
            std::fs::write(&volume_path, chunk)?;
            volumes.push(volume_path);
        }

        // 2. Extract
        let extractor = MultiVolumeExtractor::new(volumes)?;
        let output_dir = base_path.join("output");
        std::fs::create_dir(&output_dir)?;
        extractor.extract_to_with_progress(&output_dir, None, 13, 64, None::<fn(u64, u64)>)?;

        // 3. Verify
        let output_file = output_dir.join("hello.txt");
        assert!(output_file.exists());
        let content = std::fs::read_to_string(output_file)?;
        assert_eq!(content, "Hello, World!");

        Ok(())
    }
}
