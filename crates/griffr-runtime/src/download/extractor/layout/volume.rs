use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::archive_index::EOCD_MAX_SEARCH;
use super::super::range::ArchiveRangeRequest;
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub(crate) struct CachedVolumeRange {
    pub(crate) range: Range<u64>,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeLayout {
    pub(crate) path: PathBuf,
    pub(crate) url: Option<String>,
    pub(crate) start: u64,
    pub(crate) end: u64,
}

/// Unified view over local files or cached HTTP byte ranges forming a logical
/// multi-volume ZIP payload.
#[derive(Debug, Clone)]
pub struct MultiVolumeLayout {
    pub(crate) layouts: Arc<Vec<VolumeLayout>>,
    pub(crate) ranges: Arc<std::sync::RwLock<Vec<Vec<CachedVolumeRange>>>>,
    pub(crate) cache_dir: Option<PathBuf>,
    pub(crate) total_size: u64,
}

impl MultiVolumeLayout {
    fn insert_cached_range(ranges: &mut Vec<CachedVolumeRange>, cached: CachedVolumeRange) {
        if ranges
            .iter()
            .any(|existing| existing.range == cached.range && existing.path == cached.path)
        {
            return;
        }
        let index = ranges.partition_point(|existing| {
            (existing.range.start, existing.range.end) <= (cached.range.start, cached.range.end)
        });
        ranges.insert(index, cached);
    }
    pub(crate) fn from_expected(volumes: Vec<(PathBuf, u64)>) -> Result<Self> {
        if volumes.is_empty() {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: "No volumes provided".to_string(),
            });
        }
        let mut start = 0u64;
        let mut layouts = Vec::with_capacity(volumes.len());
        let mut ranges = Vec::with_capacity(volumes.len());
        for (path, size) in volumes {
            let end = start.checked_add(size).ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: "Combined archive size overflowed u64".to_string(),
            })?;
            let mut available = Vec::new();
            if std::fs::metadata(&path)
                .ok()
                .is_some_and(|metadata| metadata.len() == size)
            {
                available.push(CachedVolumeRange {
                    range: 0..size,
                    path: path.clone(),
                });
            }
            layouts.push(VolumeLayout {
                path,
                url: None,
                start,
                end,
            });
            ranges.push(available);
            start = end;
        }
        Ok(Self {
            layouts: Arc::new(layouts),
            ranges: Arc::new(std::sync::RwLock::new(ranges)),
            cache_dir: None,
            total_size: start,
        })
    }

    pub(crate) fn from_remote(
        volumes: Vec<(PathBuf, String, u64)>,
        cache_dir: PathBuf,
    ) -> Result<Self> {
        if volumes.is_empty() {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: "No volumes provided".to_string(),
            });
        }
        std::fs::create_dir_all(&cache_dir).map_err(|source| Error::IoAt {
            action: "create directory",
            path: cache_dir.clone(),
            source,
        })?;
        let mut start = 0u64;
        let mut layouts = Vec::with_capacity(volumes.len());
        let mut ranges = Vec::with_capacity(volumes.len());
        for (path, url, size) in volumes {
            let end = start.checked_add(size).ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: "Combined archive size overflowed u64".to_string(),
            })?;
            let mut available = Vec::new();
            if std::fs::metadata(&path)
                .ok()
                .is_some_and(|metadata| metadata.len() == size)
            {
                available.push(CachedVolumeRange {
                    range: 0..size,
                    path: path.clone(),
                });
            }
            layouts.push(VolumeLayout {
                path,
                url: Some(url),
                start,
                end,
            });
            ranges.push(available);
            start = end;
        }
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("range") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                let mut fields = stem.split('-');
                let Some(volume) = fields
                    .next()
                    .and_then(|value| value.strip_prefix('v'))
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    continue;
                };
                let Some(range_start) = fields.next().and_then(|value| value.parse::<u64>().ok())
                else {
                    continue;
                };
                let Some(range_end) = fields.next().and_then(|value| value.parse::<u64>().ok())
                else {
                    continue;
                };
                if fields.next().is_some()
                    || volume >= ranges.len()
                    || range_start >= range_end
                    || range_end > layouts[volume].end - layouts[volume].start
                {
                    continue;
                }
                if std::fs::metadata(&path)
                    .ok()
                    .is_some_and(|metadata| metadata.len() == range_end - range_start)
                {
                    Self::insert_cached_range(
                        &mut ranges[volume],
                        CachedVolumeRange {
                            range: range_start..range_end,
                            path,
                        },
                    );
                }
            }
        }
        Ok(Self {
            layouts: Arc::new(layouts),
            ranges: Arc::new(std::sync::RwLock::new(ranges)),
            cache_dir: Some(cache_dir),
            total_size: start,
        })
    }

    pub(crate) fn from_files(volumes: Vec<PathBuf>) -> Result<Self> {
        let mut expected = Vec::with_capacity(volumes.len());
        for path in volumes {
            let size = std::fs::metadata(&path)
                .map_err(|source| Error::IoAt {
                    action: "query file metadata/stat for",
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

    pub(crate) fn volume_range(&self, index: usize) -> Option<Range<u64>> {
        self.layouts
            .get(index)
            .map(|layout| layout.start..layout.end)
    }

    #[cfg(test)]
    pub(crate) fn volume_tail_range(&self, index: usize) -> Option<Range<u64>> {
        self.volume_range(index)
            .map(|range| range.end.saturating_sub(EOCD_MAX_SEARCH)..range.end)
    }

    pub(crate) fn total_size(&self) -> u64 {
        self.total_size
    }

    pub(crate) fn is_remote(&self) -> bool {
        self.cache_dir.is_some()
    }

    pub(crate) fn tail_probe_range(&self) -> Range<u64> {
        self.total_size.saturating_sub(EOCD_MAX_SEARCH)..self.total_size
    }

    fn refresh_local_full_volumes(&self, indices: impl IntoIterator<Item = usize>) {
        if self.is_remote() {
            return;
        }

        let mut inspected = Vec::new();
        for index in indices {
            let Some(layout) = self.layouts.get(index) else {
                continue;
            };
            let expected = layout.end - layout.start;
            let available = std::fs::metadata(&layout.path)
                .ok()
                .is_some_and(|metadata| metadata.len() == expected);
            inspected.push((index, expected, available));
        }
        if inspected.is_empty() {
            return;
        }

        let mut ranges = self.ranges.write().unwrap();
        for (index, expected, available) in inspected {
            let Some(volume) = ranges.get_mut(index) else {
                continue;
            };
            let layout = &self.layouts[index];
            if available {
                Self::insert_cached_range(
                    volume,
                    CachedVolumeRange {
                        range: 0..expected,
                        path: layout.path.clone(),
                    },
                );
            } else {
                volume.retain(|cached| cached.path != layout.path || cached.range != (0..expected));
            }
        }
    }

    fn range_covered(ranges: &[CachedVolumeRange], requested: &Range<u64>) -> bool {
        if requested.start >= requested.end {
            return true;
        }
        let mut cursor = requested.start;
        for cached in ranges {
            if cached.range.end <= cursor {
                continue;
            }
            if cached.range.start > cursor {
                return false;
            }
            cursor = cursor.max(cached.range.end);
            if cursor >= requested.end {
                return true;
            }
        }
        false
    }

    pub(crate) fn range_is_available(&self, range: &Range<u64>) -> bool {
        if range.start > range.end || range.end > self.total_size {
            return false;
        }
        let bounds = self.volume_index_bounds(range);
        self.refresh_local_full_volumes(bounds.clone());
        let ranges = self.ranges.read().unwrap();
        bounds.into_iter().all(|index| {
            let layout = &self.layouts[index];
            let start = range.start.max(layout.start);
            let end = range.end.min(layout.end);
            Self::range_covered(&ranges[index], &(start - layout.start..end - layout.start))
        })
    }

    fn volume_index_bounds(&self, range: &Range<u64>) -> Range<usize> {
        if range.start >= range.end {
            return 0..0;
        }
        let start = self
            .layouts
            .partition_point(|layout| layout.end <= range.start);
        let end = self
            .layouts
            .partition_point(|layout| layout.start < range.end);
        start..end
    }

    pub(crate) fn volume_indices_for_range(&self, range: Range<u64>) -> Vec<usize> {
        self.volume_index_bounds(&range).collect()
    }

    fn subtract_cached(requested: Range<u64>, cached: &[CachedVolumeRange]) -> Vec<Range<u64>> {
        let mut cursor = requested.start;
        let mut missing = Vec::new();
        for cached in cached {
            let range = &cached.range;
            if range.end <= cursor || range.start >= requested.end {
                continue;
            }
            if range.start > cursor {
                missing.push(cursor..range.start.min(requested.end));
            }
            cursor = cursor.max(range.end);
            if cursor >= requested.end {
                break;
            }
        }
        if cursor < requested.end {
            missing.push(cursor..requested.end);
        }
        missing
    }

    fn missing_bytes_in_range(requested: Range<u64>, cached: &[CachedVolumeRange]) -> u64 {
        let mut cursor = requested.start;
        let mut missing = 0u64;
        for cached in cached {
            let range = &cached.range;
            if range.end <= cursor || range.start >= requested.end {
                continue;
            }
            if range.start > cursor {
                missing = missing.saturating_add(range.start.min(requested.end) - cursor);
            }
            cursor = cursor.max(range.end);
            if cursor >= requested.end {
                return missing;
            }
        }
        missing.saturating_add(requested.end.saturating_sub(cursor))
    }

    /// Return the uncached byte count without allocating concrete range
    /// requests. Repair source selection calls this for every candidate archive.
    pub(crate) fn missing_bytes(&self, range: Range<u64>) -> Result<u64> {
        if range.start > range.end || range.end > self.total_size {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive byte range {}..{} exceeds stream size {}",
                    range.start, range.end, self.total_size
                ),
            });
        }
        let cached = self.ranges.read().unwrap();
        Ok(self
            .volume_index_bounds(&range)
            .map(|index| {
                let layout = &self.layouts[index];
                let start = range.start.max(layout.start) - layout.start;
                let end = range.end.min(layout.end) - layout.start;
                Self::missing_bytes_in_range(start..end, &cached[index])
            })
            .fold(0u64, u64::saturating_add))
    }

    pub(crate) fn missing_range_requests(
        &self,
        ranges: impl IntoIterator<Item = Range<u64>>,
    ) -> Result<Vec<ArchiveRangeRequest>> {
        let mut per_volume = vec![Vec::<Range<u64>>::new(); self.layouts.len()];
        for range in ranges {
            if range.start > range.end || range.end > self.total_size {
                return Err(Error::Message {
                    context: "Extraction error: ",
                    detail: format!(
                        "Archive byte range {}..{} exceeds stream size {}",
                        range.start, range.end, self.total_size
                    ),
                });
            }
            for index in self.volume_index_bounds(&range) {
                let layout = &self.layouts[index];
                let start = range.start.max(layout.start);
                let end = range.end.min(layout.end);
                per_volume[index].push(start - layout.start..end - layout.start);
            }
        }
        if !self.is_remote() {
            self.refresh_local_full_volumes(
                per_volume
                    .iter()
                    .enumerate()
                    .filter_map(|(index, requested)| (!requested.is_empty()).then_some(index)),
            );
        }

        let cached = self.ranges.read().unwrap();
        let mut requests = Vec::new();
        for (index, requested) in per_volume.into_iter().enumerate() {
            if requested.is_empty() {
                continue;
            }
            let mut requested = requested;
            requested.sort_by_key(|range| range.start);
            let mut merged = Vec::<Range<u64>>::new();
            for range in requested {
                if let Some(last) = merged.last_mut() {
                    if range.start <= last.end {
                        last.end = last.end.max(range.end);
                        continue;
                    }
                }
                merged.push(range);
            }
            let layout = &self.layouts[index];
            let Some(url) = layout.url.clone() else {
                for range in merged {
                    if !Self::range_covered(&cached[index], &range) {
                        return Err(Error::Message {
                            context: "Extraction error: ",
                            detail: format!(
                                "Local archive volume {} is missing byte range {}..{}",
                                layout.path.display(),
                                range.start,
                                range.end
                            ),
                        });
                    }
                }
                continue;
            };
            for range in merged {
                for missing in Self::subtract_cached(range, &cached[index]) {
                    let cache_dir = self.cache_dir.as_ref().ok_or_else(|| Error::Message {
                        context: "Extraction error: ",
                        detail: "Remote archive has no range-cache directory".to_string(),
                    })?;
                    let cache_path = cache_dir.join(format!(
                        "v{index:04}-{}-{}.range",
                        missing.start, missing.end
                    ));
                    requests.push(ArchiveRangeRequest {
                        volume_index: index,
                        global_range: layout.start + missing.start..layout.start + missing.end,
                        local_range: missing,
                        url: url.clone(),
                        cache_path,
                    });
                }
            }
        }
        Ok(requests)
    }

    pub(crate) fn register_range(&self, request: &ArchiveRangeRequest) -> Result<()> {
        let expected = request.local_range.end - request.local_range.start;
        let actual = std::fs::metadata(&request.cache_path)
            .map_err(|source| Error::IoAt {
                action: "query file metadata/stat for",
                path: request.cache_path.clone(),
                source,
            })?
            .len();
        if actual != expected {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive range cache {} has {actual} bytes, expected {expected}",
                    request.cache_path.display()
                ),
            });
        }
        let mut ranges = self.ranges.write().unwrap();
        let volume = ranges
            .get_mut(request.volume_index)
            .ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: "Archive range references an unknown volume".to_string(),
            })?;
        Self::insert_cached_range(
            volume,
            CachedVolumeRange {
                range: request.local_range.clone(),
                path: request.cache_path.clone(),
            },
        );
        Ok(())
    }

    pub(crate) fn register_full_volume(&self, volume_index: usize) -> Result<()> {
        let layout = self
            .layouts
            .get(volume_index)
            .ok_or_else(|| Error::Message {
                context: "Extraction error: ",
                detail: "Archive volume index is out of range".to_string(),
            })?;
        let expected = layout.end - layout.start;
        let actual = std::fs::metadata(&layout.path)
            .map_err(|source| Error::IoAt {
                action: "query file metadata/stat for",
                path: layout.path.clone(),
                source,
            })?
            .len();
        if actual != expected {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive volume {} has {actual} bytes, expected {expected}",
                    layout.path.display()
                ),
            });
        }
        let mut ranges = self.ranges.write().unwrap();
        let volume = ranges.get_mut(volume_index).ok_or_else(|| Error::Message {
            context: "Extraction error: ",
            detail: "Archive volume index is out of range".to_string(),
        })?;
        Self::insert_cached_range(
            volume,
            CachedVolumeRange {
                range: 0..expected,
                path: layout.path.clone(),
            },
        );
        Ok(())
    }

    pub(crate) fn cleanup_cache(&self) {
        if let Some(cache_dir) = self.cache_dir.as_ref() {
            if let Err(error) = std::fs::remove_dir_all(cache_dir) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %cache_dir.display(),
                        %error,
                        "failed to remove archive range cache"
                    );
                }
            }
        }
    }

    pub(crate) fn prune_range_cache(&self, still_needed: &[Range<u64>]) {
        if self.cache_dir.is_none() {
            return;
        }

        let mut ranges = self.ranges.write().unwrap();
        for (index, cached_ranges) in ranges.iter_mut().enumerate() {
            let layout = &self.layouts[index];
            cached_ranges.retain(|cached| {
                if cached.path == layout.path {
                    return true;
                }
                let global = layout.start + cached.range.start..layout.start + cached.range.end;
                let needed = still_needed
                    .iter()
                    .any(|range| range.start < global.end && range.end > global.start);
                if needed {
                    return true;
                }
                match std::fs::remove_file(&cached.path) {
                    Ok(()) => false,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        tracing::debug!(
                            path = %cached.path.display(),
                            %error,
                            "archive range is no longer needed but is still open"
                        );
                        true
                    }
                }
            });
        }
    }

    pub(crate) fn open_stream(&self) -> Result<super::stream::MultiVolumeStream> {
        super::stream::MultiVolumeStream::from_layout(self.clone())
    }

    pub(crate) fn read_range(&self, range: Range<u64>) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        if range.start > range.end || range.end > self.total_size {
            return Err(Error::Message {
                context: "Extraction error: ",
                detail: format!(
                    "Archive byte range {}..{} exceeds stream size {}",
                    range.start, range.end, self.total_size
                ),
            });
        }
        let length = usize::try_from(range.end - range.start).map_err(|_| Error::Message {
            context: "Extraction error: ",
            detail: "Archive byte range is too large for this platform".to_string(),
        })?;
        let mut stream = self.open_stream()?;
        stream.seek(SeekFrom::Start(range.start))?;
        let mut bytes = vec![0u8; length];
        stream.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}
