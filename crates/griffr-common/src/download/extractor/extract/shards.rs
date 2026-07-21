use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use md5::{Digest, Md5};

use crate::api::types::GameFileEntry;
use crate::error::{Error, Result};
use crate::runtime::preallocate_file;

use super::super::archive_index::*;
use super::index::MultiVolumeExtractor;

const MAX_STREAMING_SHARD_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STREAMING_SHARD_ENTRIES: usize = 512;
const ENTRY_METADATA_COST: u64 = 256 * 1024;

impl MultiVolumeExtractor {
    pub(crate) fn extraction_shards(
        archive_index: &ArchiveIndex,
        target_shards: usize,
    ) -> Vec<ArchiveExtractionShardPlan> {
        Self::extraction_shards_with_source_limit(
            archive_index,
            target_shards,
            MAX_STREAMING_SHARD_SOURCE_BYTES,
        )
    }

    pub(crate) fn extraction_shards_with_source_limit(
        archive_index: &ArchiveIndex,
        target_shards: usize,
        max_source_bytes: u64,
    ) -> Vec<ArchiveExtractionShardPlan> {
        let entry_count = archive_index.entry_sizes.len();
        if entry_count == 0 {
            return Vec::new();
        }
        let shard_budget = target_shards.max(1).min(entry_count);

        let mut buckets = BTreeMap::<usize, Vec<usize>>::new();
        for index in 0..entry_count {
            let release_volume = archive_index.entry_sources[index]
                .volume_indices
                .last()
                .copied()
                .unwrap_or(0);
            buckets.entry(release_volume).or_default().push(index);
        }
        for entries in buckets.values_mut() {
            entries.sort_by_key(|index| archive_index.entry_sources[*index].range.start);
        }

        let base_groups = buckets
            .into_values()
            .flat_map(|entries| {
                Self::partition_entries_by_limits(
                    archive_index,
                    entries,
                    max_source_bytes.max(1),
                    MAX_STREAMING_SHARD_ENTRIES,
                )
            })
            .collect::<Vec<_>>();
        let target_shards = shard_budget.max(base_groups.len()).min(entry_count);
        let mut allocations = vec![1usize; base_groups.len()];
        let mut remaining = target_shards.saturating_sub(base_groups.len());
        while remaining > 0 {
            let candidate = base_groups
                .iter()
                .enumerate()
                .filter(|(index, entries)| entries.len() > allocations[*index])
                .max_by_key(|(index, entries)| {
                    Self::entries_estimated_cost(archive_index, entries)
                        .div_ceil((allocations[*index] + 1) as u64)
                })
                .map(|(index, _)| index);
            let Some(index) = candidate else {
                break;
            };
            allocations[index] = allocations[index].saturating_add(1);
            remaining -= 1;
        }

        let groups = base_groups
            .into_iter()
            .zip(allocations)
            .flat_map(|(entries, parts)| Self::partition_entries(archive_index, entries, parts))
            .collect::<Vec<_>>();

        groups
            .into_iter()
            .map(|entries| Self::build_shard(archive_index, entries))
            .collect()
    }

    fn partition_entries_by_limits(
        archive_index: &ArchiveIndex,
        entries: Vec<usize>,
        max_source_bytes: u64,
        max_entries: usize,
    ) -> Vec<Vec<usize>> {
        let max_entries = max_entries.max(1);
        let mut groups = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes = 0u64;
        for entry in entries {
            let source = &archive_index.entry_sources[entry].range;
            let source_bytes = source.end.saturating_sub(source.start);
            if !current.is_empty()
                && (current_bytes.saturating_add(source_bytes) > max_source_bytes
                    || current.len() >= max_entries)
            {
                groups.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            current.push(entry);
            current_bytes = current_bytes.saturating_add(source_bytes);
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    fn partition_entries(
        archive_index: &ArchiveIndex,
        entries: Vec<usize>,
        requested_parts: usize,
    ) -> Vec<Vec<usize>> {
        let parts = requested_parts.max(1).min(entries.len());
        if parts == 1 {
            return vec![entries];
        }
        let total_cost = Self::entries_estimated_cost(archive_index, &entries);
        let target = total_cost.div_ceil(parts as u64).max(1);
        let entry_count = entries.len();
        let mut groups = Vec::with_capacity(parts);
        let mut current = Vec::new();
        let mut current_cost = 0u64;

        for (position, entry) in entries.into_iter().enumerate() {
            current_cost =
                current_cost.saturating_add(Self::entry_estimated_cost(archive_index, entry));
            current.push(entry);
            let remaining_entries = entry_count.saturating_sub(position + 1);
            let remaining_groups = parts.saturating_sub(groups.len() + 1);
            if remaining_groups > 0
                && remaining_entries >= remaining_groups
                && (current_cost >= target || remaining_entries == remaining_groups)
            {
                groups.push(std::mem::take(&mut current));
                current_cost = 0;
            }
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    fn entries_estimated_cost(archive_index: &ArchiveIndex, entries: &[usize]) -> u64 {
        entries.iter().fold(0u64, |cost, entry| {
            cost.saturating_add(Self::entry_estimated_cost(archive_index, *entry))
        })
    }

    fn entry_estimated_cost(archive_index: &ArchiveIndex, entry: usize) -> u64 {
        let compressed_bytes = archive_index.entry_compressed_sizes[entry];
        let output_bytes = archive_index.entry_sizes[entry];
        let compression_weight = match archive_index.entry_compression_methods[entry] {
            0 => 1,            // stored
            8 => 4,            // deflate
            9 => 5,            // deflate64
            12 | 14 | 95 => 6, // bzip2, lzma, xz
            93 => 3,           // zstd
            _ => 4,
        };
        compressed_bytes
            .saturating_mul(compression_weight)
            .saturating_add(output_bytes.saturating_mul(2))
            .saturating_add(ENTRY_METADATA_COST)
    }

    fn build_shard(
        archive_index: &ArchiveIndex,
        entries: Vec<usize>,
    ) -> ArchiveExtractionShardPlan {
        let volume_indices = entries
            .iter()
            .flat_map(|index| {
                archive_index.entry_sources[*index]
                    .volume_indices
                    .iter()
                    .copied()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let estimated_cost = Self::entries_estimated_cost(archive_index, &entries);
        ArchiveExtractionShardPlan {
            entries,
            volume_indices,
            estimated_cost,
        }
    }

    pub(crate) fn extract_entries_with_progress(
        &self,
        target_dir: &Path,
        password: Option<&str>,
        archive_index: &ArchiveIndex,
        entries: &[usize],
        expected_files: &BTreeMap<String, GameFileEntry>,
        progress_buffer_bytes: usize,
        mut progress_callback: impl FnMut(u64),
    ) -> Result<()> {
        let mut archive = archive_index.archive.clone();
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
            let normalized = normalized_archive_name(file.name())?;
            let expected = expected_files.get(&normalized.to_ascii_lowercase());
            let mut output =
                std::fs::File::create(&file_path).map_err(|source| Error::OpenFileFailed {
                    path: file_path.clone(),
                    source,
                })?;
            preallocate_file(&output, &file_path, file.size())?;
            let mut hasher = expected.map(|_| Md5::new());
            let mut written = 0u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                std::io::Write::write_all(&mut output, &buffer[..read])?;
                if let Some(hasher) = hasher.as_mut() {
                    hasher.update(&buffer[..read]);
                }
                written = written.saturating_add(read as u64);
                pending_progress = pending_progress.saturating_add(read as u64);
                if pending_progress >= progress_buffer_bytes as u64 {
                    progress_callback(pending_progress);
                    pending_progress = 0;
                }
            }
            if let Some(expected) = expected {
                let actual_md5 =
                    crate::to_hex(&hasher.expect("expected file has a hasher").finalize());
                if written != expected.size || actual_md5 != expected.md5.to_ascii_lowercase() {
                    let _ = std::fs::remove_file(&file_path);
                    return Err(Error::Extraction(format!(
                        "Archive entry {normalized} failed target verification: expected size {} \
                         md5 {}, got size {written} md5 {actual_md5}",
                        expected.size, expected.md5
                    )));
                }
            }
        }
        if pending_progress > 0 {
            progress_callback(pending_progress);
        }
        Ok(())
    }

    pub fn cleanup(&self) -> Result<()> {
        self.layout.cleanup_cache();
        for volume in self.layout.paths() {
            if let Err(error) = std::fs::remove_file(&volume) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Failed to delete volume {}: {}", volume.display(), error);
                }
            }
        }
        Ok(())
    }
}
