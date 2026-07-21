use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use super::layout::MultiVolumeStream;
use crate::api::types::ResourcePatch;
use crate::error::{Error, Result};

pub(crate) const EOCD_MIN_SIZE: u64 = 22;
pub(crate) const EOCD_MAX_SEARCH: u64 = EOCD_MIN_SIZE + u16::MAX as u64;
pub(crate) const CENTRAL_HEADER_SIZE: usize = 46;
pub(crate) const ZIP64_EOCD_MIN_SIZE: u64 = 56;
pub(crate) const LOCAL_HEADER_MIN_SIZE: u64 = 30;

#[derive(Debug, Clone)]
pub struct ArchiveDirectory {
    pub(crate) central_directory: Range<u64>,
    pub(crate) end_records: Range<u64>,
    pub(crate) entry_count: usize,
    pub(crate) archive_offset: u64,
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
    pub(crate) archive: zip::ZipArchive<MultiVolumeStream>,
    pub total_uncompressed_bytes: u64,
    pub(crate) entry_sizes: Vec<u64>,
    pub(crate) entry_sources: Vec<ArchiveEntrySource>,
    pub(crate) control_indices: Vec<usize>,
    pub patch_manifest: Option<ResourcePatch>,
    pub delete_manifest: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArchiveExtractionShardPlan {
    pub(crate) entries: Vec<usize>,
    pub(crate) volume_indices: Vec<usize>,
    pub(crate) uncompressed_bytes: u64,
}

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let raw = bytes.get(offset..offset + 2).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u16".to_string())
    })?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw = bytes.get(offset..offset + 4).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u32".to_string())
    })?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let raw = bytes.get(offset..offset + 8).ok_or_else(|| {
        Error::Extraction("Truncated ZIP structure while reading u64".to_string())
    })?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

pub(crate) fn open_archive_entry<'a, R: Read + Seek>(
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
                "Failed to open archive entry {index}; provide a package key for encrypted \
                 archives: {error}"
            ))
        }),
    }
}

pub(crate) fn safe_relative_archive_path(name: &str) -> Result<PathBuf> {
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

pub(crate) fn normalized_archive_name(name: &str) -> Result<String> {
    let path = safe_relative_archive_path(name)?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            Component::CurDir => None,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

pub(crate) fn parse_zip64_extra(
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
