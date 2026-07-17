//! Multi-volume zip extraction

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::api::types::ResourcePatch;
use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
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
#[derive(Debug)]
pub struct MultiVolumeStream {
    layouts: Arc<Vec<VolumeLayout>>,
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
            layouts: Arc::new(layouts),
            total_size: start,
            current_volume: 0,
            current_file: None,
            position: 0,
        };
        stream.open_current_volume()?;
        Ok(stream)
    }

    fn open_current_volume(&mut self) -> Result<()> {
        let layout = self
            .layouts
            .get(self.current_volume)
            .ok_or_else(|| Error::Extraction("No more volumes to open".to_string()))?;
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
}

impl Clone for MultiVolumeStream {
    fn clone(&self) -> Self {
        // Keep cloning infallible: the archive reader opens the selected volume
        // lazily on its first read/seek, where filesystem errors can be returned.
        Self {
            layouts: self.layouts.clone(),
            total_size: self.total_size,
            current_volume: self.current_volume,
            current_file: None,
            position: self.position,
        }
    }
}

impl Read for MultiVolumeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.current_file.is_none() && self.current_volume < self.layouts.len() {
                self.open_current_volume().map_err(std::io::Error::other)?;
                let layout = &self.layouts[self.current_volume];
                let offset = self.position.saturating_sub(layout.start);
                if let Some(file) = &mut self.current_file {
                    file.seek(SeekFrom::Start(offset))?;
                }
            }
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
            if self.current_volume != last_index || self.current_file.is_none() {
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
        if self.current_volume != index || self.current_file.is_none() {
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
    archive: zip::ZipArchive<MultiVolumeStream>,
    pub total_uncompressed_bytes: u64,
    entry_sizes: Vec<u64>,
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
        let mut entry_sizes = Vec::with_capacity(archive.len());
        let mut patch_manifest = None;
        let mut delete_manifest = None;

        for index in 0..archive.len() {
            let mut file = open_archive_entry(&mut archive, index, password)?;
            let name = normalized_archive_name(file.name())?;
            let size = if file.is_dir() { 0 } else { file.size() };
            entry_sizes.push(size);
            if name.is_empty() || file.is_dir() {
                continue;
            }
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
            archive,
            total_uncompressed_bytes,
            entry_sizes,
            patch_manifest,
            delete_manifest,
        })
    }

    pub(crate) fn range_uncompressed_bytes(
        inspection: &ArchiveInspection,
        range: Range<usize>,
    ) -> u64 {
        inspection.entry_sizes[range].iter().copied().sum()
    }

    pub(crate) fn extraction_ranges(
        inspection: &ArchiveInspection,
        max_shards: usize,
    ) -> Vec<Range<usize>> {
        let entry_count = inspection.entry_sizes.len();
        if entry_count == 0 {
            return Vec::new();
        }
        let shard_count = max_shards.max(1).min(entry_count);
        if shard_count == 1 || inspection.total_uncompressed_bytes == 0 {
            return vec![Range {
                start: 0,
                end: entry_count,
            }];
        }
        let target_bytes = inspection
            .total_uncompressed_bytes
            .div_ceil(shard_count as u64)
            .max(1);
        let mut ranges = Vec::with_capacity(shard_count);
        let mut start = 0usize;
        let mut accumulated = 0u64;
        for (index, size) in inspection.entry_sizes.iter().copied().enumerate() {
            accumulated = accumulated.saturating_add(size);
            let remaining_entries = entry_count.saturating_sub(index + 1);
            let remaining_shards = shard_count.saturating_sub(ranges.len() + 1);
            if accumulated >= target_bytes
                && remaining_shards > 0
                && remaining_entries >= remaining_shards
            {
                ranges.push(start..index + 1);
                start = index + 1;
                accumulated = 0;
            }
        }
        if start < entry_count {
            ranges.push(start..entry_count);
        }
        ranges
    }

    pub(crate) fn extract_range_with_progress(
        &self,
        target_dir: &Path,
        password: Option<&str>,
        inspection: &ArchiveInspection,
        range: Range<usize>,
        progress_buffer_bytes: usize,
        mut progress_callback: impl FnMut(u64),
    ) -> Result<()> {
        // zip 2.x keeps parsed central-directory state in an Arc, so cloning
        // this inspected archive gives each scheduler shard an independent
        // lazy stream while sharing immutable central-directory state.
        let mut archive = inspection.archive.clone();
        let mut buffer = vec![0u8; progress_buffer_bytes.max(4 * 1024)];
        let mut pending_progress = 0u64;

        for index in range {
            let mut file = open_archive_entry(&mut archive, index, password)?;
            let file_path = target_dir.join(safe_relative_archive_path(file.name())?);
            if file.is_dir() {
                std::fs::create_dir_all(&file_path).map_err(|source| Error::CreateDirFailed {
                    path: file_path.clone(),
                    source,
                })?;
                continue;
            }
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| Error::CreateDirFailed {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            let mut output =
                std::fs::File::create(&file_path).map_err(|source| Error::OpenFileFailed {
                    path: file_path.clone(),
                    source,
                })?;
            preallocate_file(&output, &file_path, file.size())?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &buffer[..read])?;
                pending_progress = pending_progress.saturating_add(read as u64);
                if pending_progress >= progress_buffer_bytes as u64 {
                    progress_callback(pending_progress);
                    pending_progress = 0;
                }
            }
        }
        if pending_progress > 0 {
            progress_callback(pending_progress);
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
mod tests;
