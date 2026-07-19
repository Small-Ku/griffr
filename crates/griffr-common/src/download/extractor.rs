//! Multi-volume ZIP indexing and range-aware extraction.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::api::types::ResourcePatch;
use crate::error::{Error, Result};
use crate::runtime::preallocate_file;
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME};

const EOCD_MIN_SIZE: u64 = 22;
const EOCD_MAX_SEARCH: u64 = EOCD_MIN_SIZE + u16::MAX as u64;
const ZIP64_EOCD_MIN_SIZE: u64 = 56;
const CENTRAL_HEADER_SIZE: usize = 46;
const LOCAL_HEADER_MIN_SIZE: u64 = 30;

#[derive(Debug, Clone)]
struct VolumeLayout {
    path: PathBuf,
    start: u64,
    end: u64,
}

/// Immutable expected layout of an externally split ZIP stream. Expected sizes
/// come from the package manifest, so tail and central-directory reads do not
/// require every earlier volume to exist yet.
#[derive(Debug, Clone)]
pub(crate) struct MultiVolumeLayout {
    layouts: Arc<Vec<VolumeLayout>>,
    total_size: u64,
}

impl MultiVolumeLayout {
    pub(crate) fn from_expected(volumes: Vec<(PathBuf, u64)>) -> Result<Self> {
        if volumes.is_empty() {
            return Err(Error::Extraction("No volumes provided".to_string()));
        }
        let mut start = 0u64;
        let mut layouts = Vec::with_capacity(volumes.len());
        for (path, size) in volumes {
            let end = start.checked_add(size).ok_or_else(|| {
                Error::Extraction("Combined archive size overflowed u64".to_string())
            })?;
            layouts.push(VolumeLayout { path, start, end });
            start = end;
        }
        Ok(Self {
            layouts: Arc::new(layouts),
            total_size: start,
        })
    }

    pub(crate) fn from_files(volumes: Vec<PathBuf>) -> Result<Self> {
        let mut expected = Vec::with_capacity(volumes.len());
        for path in volumes {
            let size = std::fs::metadata(&path)
                .map_err(|source| Error::StatFailed {
                    path: path.clone(),
                    source,
                })?
                .len();
            expected.push((path, size));
        }
        Self::from_expected(expected)
    }

    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        self.layouts
            .iter()
            .map(|layout| layout.path.clone())
            .collect()
    }

    pub(crate) fn path(&self, index: usize) -> Option<&Path> {
        self.layouts.get(index).map(|layout| layout.path.as_path())
    }

    pub(crate) fn volume_count(&self) -> usize {
        self.layouts.len()
    }

    pub(crate) fn total_size(&self) -> u64 {
        self.total_size
    }

    pub(crate) fn tail_probe_range(&self) -> Range<u64> {
        self.total_size.saturating_sub(EOCD_MAX_SEARCH)..self.total_size
    }

    pub(crate) fn range_is_available(&self, range: &Range<u64>) -> bool {
        if range.start > range.end || range.end > self.total_size {
            return false;
        }
        self.volume_indices_for_range(range.clone())
            .into_iter()
            .all(|index| {
                let layout = &self.layouts[index];
                std::fs::metadata(&layout.path)
                    .ok()
                    .is_some_and(|metadata| metadata.len() == layout.end - layout.start)
            })
    }

    pub(crate) fn volume_indices_for_range(&self, range: Range<u64>) -> Vec<usize> {
        if range.start >= range.end {
            return Vec::new();
        }
        self.layouts
            .iter()
            .enumerate()
            .filter_map(|(index, layout)| {
                (layout.start < range.end && layout.end > range.start).then_some(index)
            })
            .collect()
    }

    fn open_stream(&self) -> Result<MultiVolumeStream> {
        MultiVolumeStream::from_layout(self.clone())
    }

    fn read_range(&self, range: Range<u64>) -> Result<Vec<u8>> {
        if range.start > range.end || range.end > self.total_size {
            return Err(Error::Extraction(format!(
                "Archive byte range {}..{} exceeds stream size {}",
                range.start, range.end, self.total_size
            )));
        }
        let length = usize::try_from(range.end - range.start).map_err(|_| {
            Error::Extraction("Archive byte range is too large for this platform".to_string())
        })?;
        let mut stream = self.open_stream()?;
        stream.seek(SeekFrom::Start(range.start))?;
        let mut bytes = vec![0u8; length];
        stream.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

/// Seekable view over expected split-volume offsets. Only the volume touched by
/// a read is opened, which permits central-directory planning while unrelated
/// package parts are still downloading.
#[derive(Debug)]
pub struct MultiVolumeStream {
    layout: MultiVolumeLayout,
    current_volume: usize,
    current_file: Option<std::fs::File>,
    position: u64,
}

impl MultiVolumeStream {
    fn from_layout(layout: MultiVolumeLayout) -> Result<Self> {
        if layout.layouts.is_empty() {
            return Err(Error::Extraction("No volumes provided".to_string()));
        }
        Ok(Self {
            layout,
            current_volume: 0,
            current_file: None,
            position: 0,
        })
    }

    fn open_current_volume(&mut self) -> Result<()> {
        let layout = self
            .layout
            .layouts
            .get(self.current_volume)
            .ok_or_else(|| Error::Extraction("No more volumes to open".to_string()))?;
        let file = std::fs::File::open(&layout.path).map_err(|source| Error::OpenFileFailed {
            path: layout.path.clone(),
            source,
        })?;
        let actual = file
            .metadata()
            .map_err(|source| Error::StatFailed {
                path: layout.path.clone(),
                source,
            })?
            .len();
        let expected = layout.end - layout.start;
        if actual != expected {
            return Err(Error::Extraction(format!(
                "Archive volume {} has size {actual}, expected {expected}",
                layout.path.display()
            )));
        }
        self.current_file = Some(file);
        Ok(())
    }

    fn select_volume(&mut self, index: usize, offset: u64) -> std::io::Result<()> {
        if self.current_volume != index || self.current_file.is_none() {
            self.current_volume = index;
            self.open_current_volume().map_err(std::io::Error::other)?;
        }
        if let Some(file) = &mut self.current_file {
            file.seek(SeekFrom::Start(offset))?;
        }
        Ok(())
    }
}

impl Clone for MultiVolumeStream {
    fn clone(&self) -> Self {
        Self {
            layout: self.layout.clone(),
            current_volume: self.current_volume,
            current_file: None,
            position: self.position,
        }
    }
}

impl Read for MultiVolumeStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.position == self.layout.total_size {
            return Ok(0);
        }
        let index = self
            .layout
            .layouts
            .partition_point(|volume| volume.end <= self.position);
        let (volume_start, volume_end, volume_path) = {
            let volume = self.layout.layouts.get(index).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "archive stream ended")
            })?;
            (volume.start, volume.end, volume.path.clone())
        };
        let offset = self.position - volume_start;
        let remaining = usize::try_from(volume_end - self.position).unwrap_or(usize::MAX);
        self.select_volume(index, offset)?;
        let limit = remaining.min(buf.len());
        let read = self
            .current_file
            .as_mut()
            .expect("selected archive volume is open")
            .read(&mut buf[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("archive volume {} ended early", volume_path.display()),
            ));
        }
        self.position = self.position.saturating_add(read as u64);
        Ok(read)
    }
}

impl Seek for MultiVolumeStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.layout.total_size) + i128::from(offset),
        };
        if target < 0 || target > i128::from(self.layout.total_size) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek outside archive stream",
            ));
        }
        self.position = target as u64;
        self.current_file = None;
        Ok(self.position)
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveDirectory {
    pub(crate) central_directory: Range<u64>,
    pub(crate) end_records: Range<u64>,
    pub(crate) entry_count: usize,
    archive_offset: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum ArchiveDirectoryDiscovery {
    Ready(ArchiveDirectory),
    NeedsRange(Range<u64>),
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveEntrySource {
    pub(crate) range: Range<u64>,
    pub(crate) volume_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct ArchiveInspection {
    pub entries: BTreeMap<String, u64>,
    archive: zip::ZipArchive<MultiVolumeStream>,
    pub total_uncompressed_bytes: u64,
    entry_sizes: Vec<u64>,
    entry_sources: Vec<ArchiveEntrySource>,
    control_indices: Vec<usize>,
    pub patch_manifest: Option<ResourcePatch>,
    pub delete_manifest: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveExtractionShardPlan {
    pub(crate) entries: Vec<usize>,
    pub(crate) volume_indices: Vec<usize>,
    pub(crate) uncompressed_bytes: u64,
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let raw = bytes.get(offset..offset + 2).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u16".to_string())
    })?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw = bytes.get(offset..offset + 4).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u32".to_string())
    })?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let raw = bytes.get(offset..offset + 8).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u64".to_string())
    })?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

fn open_archive_entry<'a, R: Read + Seek>(
    archive: &'a mut zip::ZipArchive<R>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a, R>> {
    match password {
        Some(password) => archive
            .by_index_decrypt(index, password.as_bytes())
            .map_err(|error| {
                Error::Extraction(format!(
                    "Failed to decrypt archive entry {index}; package key may be incorrect: {error}"
                ))
            }),
        None => archive.by_index(index).map_err(|error| {
            Error::Extraction(format!(
                "Failed to open archive entry {index}; provide a package key for encrypted archives: {error}"
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
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err(Error::InvalidPath(format!(
                "Zip entry has unsafe path: {name}"
            )));
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

fn parse_zip64_extra(
    extra: &[u8],
    needs_uncompressed: bool,
    needs_compressed: bool,
    needs_offset: bool,
    needs_disk_start: bool,
) -> Result<(Option<u64>, Option<u64>, Option<u64>, Option<u32>)> {
    let mut cursor = 0usize;
    while cursor + 4 <= extra.len() {
        let kind = read_u16(extra, cursor)?;
        let size = usize::from(read_u16(extra, cursor + 2)?);
        cursor += 4;
        let payload = extra
            .get(cursor..cursor + size)
            .ok_or_else(|| Error::Extraction("Truncated ZIP extra field".to_string()))?;
        if kind == 0x0001 {
            let mut field = 0usize;
            let mut next_u64 = || -> Result<u64> {
                let value = read_u64(payload, field)?;
                field += 8;
                Ok(value)
            };
            let uncompressed = if needs_uncompressed {
                Some(next_u64()?)
            } else {
                None
            };
            let compressed = if needs_compressed {
                Some(next_u64()?)
            } else {
                None
            };
            let offset = if needs_offset {
                Some(next_u64()?)
            } else {
                None
            };
            let disk_start = if needs_disk_start {
                Some(read_u32(payload, field)?)
            } else {
                None
            };
            return Ok((uncompressed, compressed, offset, disk_start));
        }
        cursor += size;
    }
    Ok((None, None, None, None))
}

pub(crate) struct MultiVolumeExtractor {
    layout: MultiVolumeLayout,
}

impl MultiVolumeExtractor {
    pub(crate) fn new(volumes: Vec<PathBuf>) -> Result<Self> {
        Ok(Self {
            layout: MultiVolumeLayout::from_files(volumes)?,
        })
    }

    pub(crate) fn from_layout(layout: MultiVolumeLayout) -> Self {
        Self { layout }
    }

    pub(crate) fn discover_archive_directory(&self) -> Result<ArchiveDirectoryDiscovery> {
        let tail_range = self.layout.tail_probe_range();
        if !self.layout.range_is_available(&tail_range) {
            return Ok(ArchiveDirectoryDiscovery::NeedsRange(tail_range));
        }
        let tail = self.layout.read_range(tail_range.clone())?;
        let mut eocd_offset = None;
        for offset in (0..=tail.len().saturating_sub(EOCD_MIN_SIZE as usize)).rev() {
            if tail.get(offset..offset + 4) != Some(&[0x50, 0x4b, 0x05, 0x06]) {
                continue;
            }
            let comment = usize::from(read_u16(&tail, offset + 20)?);
            if offset + EOCD_MIN_SIZE as usize + comment == tail.len() {
                eocd_offset = Some(offset);
                break;
            }
        }
        let eocd_offset = eocd_offset.ok_or_else(|| {
            Error::Extraction("ZIP end-of-central-directory record was not found".to_string())
        })?;
        let eocd_start = tail_range.start + eocd_offset as u64;
        let disk_number = read_u16(&tail, eocd_offset + 4)?;
        let central_disk = read_u16(&tail, eocd_offset + 6)?;
        let entries_on_disk_16 = read_u16(&tail, eocd_offset + 8)?;
        let total_entries_16 = read_u16(&tail, eocd_offset + 10)?;
        let central_size_32 = read_u32(&tail, eocd_offset + 12)?;
        let central_offset_32 = read_u32(&tail, eocd_offset + 16)?;
        if disk_number != 0 || central_disk != 0 {
            return Err(Error::Extraction(
                "Spanned multi-disk ZIP archives are not supported; expected raw split volumes"
                    .to_string(),
            ));
        }

        let zip64 = entries_on_disk_16 == u16::MAX
            || total_entries_16 == u16::MAX
            || central_size_32 == u32::MAX
            || central_offset_32 == u32::MAX;
        if !zip64 && entries_on_disk_16 != total_entries_16 {
            return Err(Error::Extraction(
                "ZIP entry counts differ across disks; spanned archives are unsupported"
                    .to_string(),
            ));
        }

        let (entry_count, central_size, relative_central_start, end_start) = if zip64 {
            let locator_start = eocd_start
                .checked_sub(20)
                .ok_or_else(|| Error::Extraction("ZIP64 locator is missing".to_string()))?;
            let locator_range = locator_start..eocd_start;
            if !self.layout.range_is_available(&locator_range) {
                return Ok(ArchiveDirectoryDiscovery::NeedsRange(locator_range));
            }
            let locator = self.layout.read_range(locator_range)?;
            if locator.get(0..4) != Some(&[0x50, 0x4b, 0x06, 0x07]) {
                return Err(Error::Extraction("ZIP64 locator is invalid".to_string()));
            }
            if read_u32(&locator, 4)? != 0 || read_u32(&locator, 16)? != 1 {
                return Err(Error::Extraction(
                    "Spanned ZIP64 archives are not supported; expected one logical disk"
                        .to_string(),
                ));
            }
            let zip64_start = read_u64(&locator, 8)?;
            let zip64_end = zip64_start
                .checked_add(ZIP64_EOCD_MIN_SIZE)
                .ok_or_else(|| {
                    Error::Extraction("ZIP64 end-record range overflowed".to_string())
                })?;
            if zip64_end > self.layout.total_size() {
                return Err(Error::Extraction(
                    "ZIP64 end record lies outside the archive stream".to_string(),
                ));
            }
            let required = zip64_start..zip64_end;
            if !self.layout.range_is_available(&required) {
                return Ok(ArchiveDirectoryDiscovery::NeedsRange(required));
            }
            let record = self.layout.read_range(required)?;
            if record.get(0..4) != Some(&[0x50, 0x4b, 0x06, 0x06]) {
                return Err(Error::Extraction("ZIP64 end record is invalid".to_string()));
            }
            if read_u32(&record, 16)? != 0 || read_u32(&record, 20)? != 0 {
                return Err(Error::Extraction(
                    "Spanned ZIP64 archives are not supported; expected raw split volumes"
                        .to_string(),
                ));
            }
            let entries_on_disk = read_u64(&record, 24)?;
            let total_entries = read_u64(&record, 32)?;
            if entries_on_disk != total_entries {
                return Err(Error::Extraction(
                    "ZIP64 entry counts differ across disks; spanned archives are unsupported"
                        .to_string(),
                ));
            }
            (
                usize::try_from(total_entries)
                    .map_err(|_| Error::Extraction("ZIP entry count exceeds usize".to_string()))?,
                read_u64(&record, 40)?,
                read_u64(&record, 48)?,
                zip64_start,
            )
        } else {
            (
                usize::from(total_entries_16),
                u64::from(central_size_32),
                u64::from(central_offset_32),
                eocd_start,
            )
        };

        let central_start = end_start.checked_sub(central_size).ok_or_else(|| {
            Error::Extraction("ZIP central directory is larger than its archive prefix".to_string())
        })?;
        if central_start < relative_central_start {
            return Err(Error::Extraction(
                "ZIP central-directory offset is inconsistent with the end record".to_string(),
            ));
        }
        if end_start > self.layout.total_size() {
            return Err(Error::Extraction(
                "ZIP central directory lies outside the archive stream".to_string(),
            ));
        }
        Ok(ArchiveDirectoryDiscovery::Ready(ArchiveDirectory {
            central_directory: central_start..end_start,
            end_records: end_start..self.layout.total_size(),
            entry_count,
            archive_offset: central_start - relative_central_start,
        }))
    }

    pub(crate) fn inspect_archive_index(
        &self,
        directory: &ArchiveDirectory,
    ) -> Result<ArchiveInspection> {
        // ZipArchive::new reads the end records and central directory but does
        // not need to open local file headers. The layout therefore may still
        // contain unavailable earlier volumes at this planning stage.
        let archive = zip::ZipArchive::new(self.layout.open_stream()?)?;
        if archive.len() != directory.entry_count {
            return Err(Error::Extraction(format!(
                "ZIP central directory reported {} entries, parser found {}",
                directory.entry_count,
                archive.len()
            )));
        }
        let central_start = directory.central_directory.start;
        let central_size = directory.central_directory.end - directory.central_directory.start;
        let central_end = central_start.checked_add(central_size).ok_or_else(|| {
            Error::Extraction("ZIP central-directory range overflowed".to_string())
        })?;
        let central = self.layout.read_range(central_start..central_end)?;
        let archive_offset = directory.archive_offset;
        let mut cursor = 0usize;
        let mut entries = BTreeMap::new();
        let mut entry_sizes = Vec::with_capacity(directory.entry_count);
        let mut compressed_sizes = Vec::with_capacity(directory.entry_count);
        let mut starts = Vec::with_capacity(directory.entry_count);
        let mut control_indices = Vec::new();
        let mut total_uncompressed_bytes = 0u64;

        for index in 0..directory.entry_count {
            if central.get(cursor..cursor + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
                return Err(Error::Extraction(format!(
                    "Invalid central-directory record at entry {index}"
                )));
            }
            let compressed_32 = read_u32(&central, cursor + 20)?;
            let uncompressed_32 = read_u32(&central, cursor + 24)?;
            let name_len = usize::from(read_u16(&central, cursor + 28)?);
            let extra_len = usize::from(read_u16(&central, cursor + 30)?);
            let comment_len = usize::from(read_u16(&central, cursor + 32)?);
            let disk_start_16 = read_u16(&central, cursor + 34)?;
            let local_offset_32 = read_u32(&central, cursor + 42)?;
            let name_start = cursor.checked_add(CENTRAL_HEADER_SIZE).ok_or_else(|| {
                Error::Extraction("ZIP central-directory offset overflowed".to_string())
            })?;
            let name_end = name_start
                .checked_add(name_len)
                .ok_or_else(|| Error::Extraction("ZIP filename range overflowed".to_string()))?;
            let extra_end = name_end
                .checked_add(extra_len)
                .ok_or_else(|| Error::Extraction("ZIP extra-data range overflowed".to_string()))?;
            let record_end = extra_end.checked_add(comment_len).ok_or_else(|| {
                Error::Extraction("ZIP central record range overflowed".to_string())
            })?;
            central
                .get(name_start..name_end)
                .ok_or_else(|| Error::Extraction("Truncated ZIP filename".to_string()))?;
            let extra = central
                .get(name_end..extra_end)
                .ok_or_else(|| Error::Extraction("Truncated ZIP extra data".to_string()))?;
            central
                .get(extra_end..record_end)
                .ok_or_else(|| Error::Extraction("Truncated ZIP file comment".to_string()))?;

            let name = archive.name_for_index(index).ok_or_else(|| {
                Error::Extraction(format!("ZIP parser has no name for entry {index}"))
            })?;
            let normalized = normalized_archive_name(name)?;
            let (zip64_uncompressed, zip64_compressed, zip64_offset, zip64_disk_start) =
                parse_zip64_extra(
                    extra,
                    uncompressed_32 == u32::MAX,
                    compressed_32 == u32::MAX,
                    local_offset_32 == u32::MAX,
                    disk_start_16 == u16::MAX,
                )?;
            let size = if uncompressed_32 == u32::MAX {
                zip64_uncompressed.ok_or_else(|| {
                    Error::Extraction(format!(
                        "ZIP64 entry {normalized} is missing its uncompressed size"
                    ))
                })?
            } else {
                u64::from(uncompressed_32)
            };
            let compressed_size = if compressed_32 == u32::MAX {
                zip64_compressed.ok_or_else(|| {
                    Error::Extraction(format!(
                        "ZIP64 entry {normalized} is missing its compressed size"
                    ))
                })?
            } else {
                u64::from(compressed_32)
            };
            let local_offset = if local_offset_32 == u32::MAX {
                zip64_offset.ok_or_else(|| {
                    Error::Extraction(format!(
                        "ZIP64 entry {normalized} is missing its local-header offset"
                    ))
                })?
            } else {
                u64::from(local_offset_32)
            };
            let disk_start = if disk_start_16 == u16::MAX {
                zip64_disk_start.ok_or_else(|| {
                    Error::Extraction(format!(
                        "ZIP64 entry {normalized} is missing its start-disk number"
                    ))
                })?
            } else {
                u32::from(disk_start_16)
            };
            if disk_start != 0 {
                return Err(Error::Extraction(format!(
                    "ZIP entry {normalized} starts on disk {disk_start}; spanned archives are unsupported"
                )));
            }

            let absolute_start = archive_offset.checked_add(local_offset).ok_or_else(|| {
                Error::Extraction("ZIP local-header offset overflowed".to_string())
            })?;
            if absolute_start >= central_start {
                return Err(Error::Extraction(format!(
                    "ZIP entry {normalized} starts inside the central directory"
                )));
            }
            starts.push((absolute_start, index));
            compressed_sizes.push(compressed_size);
            let is_directory = name.ends_with('/');
            entry_sizes.push(if is_directory { 0 } else { size });
            if !normalized.is_empty() && !is_directory {
                total_uncompressed_bytes = total_uncompressed_bytes.saturating_add(size);
                if entries.insert(normalized.clone(), size).is_some() {
                    return Err(Error::Extraction(format!(
                        "Archive contains duplicate entry {normalized}"
                    )));
                }
                if normalized == PATCH_MANIFEST_NAME || normalized == DELETE_FILES_MANIFEST_NAME {
                    control_indices.push(index);
                }
            }
            cursor = record_end;
        }

        starts.sort_unstable();
        for pair in starts.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(Error::Extraction(
                    "ZIP entries share a local-header offset".to_string(),
                ));
            }
        }
        let mut entry_sources = vec![
            ArchiveEntrySource {
                range: 0..0,
                volume_indices: Vec::new(),
            };
            directory.entry_count
        ];
        for (position, (start, index)) in starts.iter().copied().enumerate() {
            let end = starts
                .get(position + 1)
                .map(|next| next.0)
                .unwrap_or(central_start);
            let minimum_end = start
                .checked_add(LOCAL_HEADER_MIN_SIZE)
                .and_then(|value| value.checked_add(compressed_sizes[index]))
                .ok_or_else(|| {
                    Error::Extraction(format!(
                        "ZIP entry {} source range overflowed",
                        archive.name_for_index(index).unwrap_or("<unknown>")
                    ))
                })?;
            if minimum_end > end {
                return Err(Error::Extraction(format!(
                    "ZIP entry {} overlaps the next local header",
                    archive.name_for_index(index).unwrap_or("<unknown>")
                )));
            }
            let range = start..end;
            entry_sources[index] = ArchiveEntrySource {
                volume_indices: self.layout.volume_indices_for_range(range.clone()),
                range,
            };
        }

        Ok(ArchiveInspection {
            entries,
            archive,
            total_uncompressed_bytes,
            entry_sizes,
            entry_sources,
            control_indices,
            patch_manifest: None,
            delete_manifest: None,
        })
    }

    pub(crate) fn control_volume_indices(inspection: &ArchiveInspection) -> Vec<usize> {
        inspection
            .control_indices
            .iter()
            .flat_map(|index| {
                inspection.entry_sources[*index]
                    .volume_indices
                    .iter()
                    .copied()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn read_control_payloads(
        &self,
        inspection: &ArchiveInspection,
        password: Option<&str>,
    ) -> Result<ArchiveInspection> {
        const MAX_CONTROL_FILE_BYTES: u64 = 16 * 1024 * 1024;
        let mut result = inspection.clone();
        let mut archive = inspection.archive.clone();
        for index in &inspection.control_indices {
            let mut file = open_archive_entry(&mut archive, *index, password)?;
            let name = normalized_archive_name(file.name())?;
            if file.size() > MAX_CONTROL_FILE_BYTES {
                return Err(Error::Extraction(format!(
                    "Archive control file {name} is unexpectedly large ({} bytes)",
                    file.size()
                )));
            }
            let mut payload = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut payload)?;
            if name == PATCH_MANIFEST_NAME {
                result.patch_manifest = Some(serde_json::from_slice(&payload)?);
            } else if name == DELETE_FILES_MANIFEST_NAME {
                result.delete_manifest = Some(String::from_utf8(payload).map_err(|error| {
                    Error::Extraction(format!(
                        "{DELETE_FILES_MANIFEST_NAME} is not UTF-8: {error}"
                    ))
                })?);
            }
        }
        Ok(result)
    }

    pub(crate) fn inspect_patch_payload(
        &self,
        password: Option<&str>,
    ) -> Result<ArchiveInspection> {
        let directory = match self.discover_archive_directory()? {
            ArchiveDirectoryDiscovery::Ready(directory) => directory,
            ArchiveDirectoryDiscovery::NeedsRange(range) => {
                return Err(Error::Extraction(format!(
                    "Archive directory needs unavailable byte range {}..{}",
                    range.start, range.end
                )))
            }
        };
        let inspection = self.inspect_archive_index(&directory)?;
        self.read_control_payloads(&inspection, password)
    }

    pub(crate) fn extraction_shards(
        inspection: &ArchiveInspection,
        max_shards: usize,
    ) -> Vec<ArchiveExtractionShardPlan> {
        let entry_count = inspection.entry_sizes.len();
        if entry_count == 0 {
            return Vec::new();
        }
        let shard_budget = max_shards.max(1).min(entry_count);

        // Entries that become readable at the same latest volume form one
        // release bucket. Keeping these boundaries when the shard budget
        // permits prevents early entries from being held behind unrelated late
        // package parts merely to balance uncompressed byte counts.
        let mut buckets = BTreeMap::<usize, Vec<usize>>::new();
        for index in 0..entry_count {
            let release_volume = inspection.entry_sources[index]
                .volume_indices
                .last()
                .copied()
                .unwrap_or(0);
            buckets.entry(release_volume).or_default().push(index);
        }
        for entries in buckets.values_mut() {
            entries.sort_by_key(|index| inspection.entry_sources[*index].range.start);
        }

        let bucket_entries = buckets.into_values().collect::<Vec<_>>();
        let groups = if bucket_entries.len() <= shard_budget {
            let mut allocations = vec![1usize; bucket_entries.len()];
            let mut remaining = shard_budget.saturating_sub(bucket_entries.len());
            while remaining > 0 {
                let candidate = bucket_entries
                    .iter()
                    .enumerate()
                    .filter(|(index, entries)| entries.len() > allocations[*index])
                    .max_by_key(|(index, entries)| {
                        let bytes = entries
                            .iter()
                            .map(|entry| inspection.entry_sizes[*entry])
                            .sum::<u64>();
                        bytes
                            .div_ceil((allocations[*index] + 1) as u64)
                            .max(entries.len() as u64)
                    })
                    .map(|(index, _)| index);
                let Some(index) = candidate else {
                    break;
                };
                allocations[index] = allocations[index].saturating_add(1);
                remaining -= 1;
            }

            bucket_entries
                .into_iter()
                .zip(allocations)
                .flat_map(|(entries, parts)| Self::partition_entries(inspection, entries, parts))
                .collect::<Vec<_>>()
        } else {
            Self::merge_release_buckets(inspection, bucket_entries, shard_budget)
        };

        groups
            .into_iter()
            .map(|entries| {
                let bytes = entries
                    .iter()
                    .map(|index| inspection.entry_sizes[*index])
                    .sum();
                Self::build_shard(inspection, entries, bytes)
            })
            .collect()
    }

    fn merge_release_buckets(
        inspection: &ArchiveInspection,
        buckets: Vec<Vec<usize>>,
        shard_count: usize,
    ) -> Vec<Vec<usize>> {
        let total_bytes = buckets
            .iter()
            .flatten()
            .map(|index| inspection.entry_sizes[*index])
            .sum::<u64>();
        let target = total_bytes.div_ceil(shard_count as u64).max(1);
        let bucket_count = buckets.len();
        let mut groups = Vec::with_capacity(shard_count);
        let mut current = Vec::new();
        let mut current_bytes = 0u64;

        for (position, bucket) in buckets.into_iter().enumerate() {
            current_bytes = current_bytes.saturating_add(
                bucket
                    .iter()
                    .map(|index| inspection.entry_sizes[*index])
                    .sum::<u64>(),
            );
            current.extend(bucket);
            let remaining_buckets = bucket_count.saturating_sub(position + 1);
            let remaining_groups = shard_count.saturating_sub(groups.len() + 1);
            if remaining_groups > 0
                && remaining_buckets >= remaining_groups
                && (current_bytes >= target || remaining_buckets == remaining_groups)
            {
                groups.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    fn partition_entries(
        inspection: &ArchiveInspection,
        entries: Vec<usize>,
        requested_parts: usize,
    ) -> Vec<Vec<usize>> {
        let parts = requested_parts.max(1).min(entries.len());
        if parts == 1 {
            return vec![entries];
        }
        let total_bytes = entries
            .iter()
            .map(|index| inspection.entry_sizes[*index])
            .sum::<u64>();
        let target = total_bytes.div_ceil(parts as u64).max(1);
        let entry_count = entries.len();
        let mut groups = Vec::with_capacity(parts);
        let mut current = Vec::new();
        let mut current_bytes = 0u64;

        for (position, entry) in entries.into_iter().enumerate() {
            current_bytes = current_bytes.saturating_add(inspection.entry_sizes[entry]);
            current.push(entry);
            let remaining_entries = entry_count.saturating_sub(position + 1);
            let remaining_groups = parts.saturating_sub(groups.len() + 1);
            if remaining_groups > 0
                && remaining_entries >= remaining_groups
                && (current_bytes >= target || remaining_entries == remaining_groups)
            {
                groups.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    fn build_shard(
        inspection: &ArchiveInspection,
        entries: Vec<usize>,
        uncompressed_bytes: u64,
    ) -> ArchiveExtractionShardPlan {
        let volume_indices = entries
            .iter()
            .flat_map(|index| {
                inspection.entry_sources[*index]
                    .volume_indices
                    .iter()
                    .copied()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        ArchiveExtractionShardPlan {
            entries,
            volume_indices,
            uncompressed_bytes,
        }
    }

    pub(crate) fn extract_entries_with_progress(
        &self,
        target_dir: &Path,
        password: Option<&str>,
        inspection: &ArchiveInspection,
        entries: &[usize],
        progress_buffer_bytes: usize,
        mut progress_callback: impl FnMut(u64),
    ) -> Result<()> {
        let mut archive = inspection.archive.clone();
        let mut buffer = vec![0u8; progress_buffer_bytes.max(4 * 1024)];
        let mut pending_progress = 0u64;
        for index in entries {
            let mut file = open_archive_entry(&mut archive, *index, password)?;
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

    pub fn cleanup(&self) -> Result<()> {
        for volume in self.layout.paths() {
            if let Err(error) = std::fs::remove_file(&volume) {
                tracing::warn!("Failed to delete volume {}: {}", volume.display(), error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
