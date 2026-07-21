use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::ops::Range;

use crate::error::{Error, Result};
use crate::runtime::{DELETE_FILES_MANIFEST_NAME, PATCH_MANIFEST_NAME};

use super::super::inspection::*;
use super::super::layout::MultiVolumeLayout;

const EOCD_MIN_SIZE: u64 = 22;

pub struct MultiVolumeExtractor {
    pub(crate) layout: MultiVolumeLayout,
}

impl MultiVolumeExtractor {
    pub(crate) fn new(volumes: Vec<std::path::PathBuf>) -> Result<Self> {
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
        let parsed_archive = zip::ZipArchive::new(self.layout.open_stream()?)?;
        let archive = parsed_archive.clone();
        drop(parsed_archive);
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
                    "ZIP entry {normalized} starts on disk {disk_start}; spanned archives are \
                     unsupported"
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

    pub(crate) fn source_ranges_for_indices(
        inspection: &ArchiveInspection,
        indices: &[usize],
    ) -> Vec<Range<u64>> {
        indices
            .iter()
            .filter_map(|index| inspection.entry_sources.get(*index))
            .map(|source| source.range.clone())
            .collect()
    }

    pub(crate) fn control_source_ranges(inspection: &ArchiveInspection) -> Vec<Range<u64>> {
        Self::source_ranges_for_indices(inspection, &inspection.control_indices)
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
}
