use crate::api::types::GameFileEntry;
use crate::download::extractor::{ArchiveIndex, MultiVolumeLayout};
use crate::error::{Error, Result};
use crate::runtime::{PatchApplyOptions, PatchCheckReport, PatchPlan};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::runtime::task_pool::graph::TaskDependencyToken;

use super::tasks::{ArchivePart, ArchiveRetention};

#[derive(Debug, Clone)]
pub(crate) struct PreparedArchive {
    pub(crate) staging_dir: PathBuf,
    pub(crate) patch_plan: Option<(PatchPlan, PatchCheckReport)>,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ArchiveWork {
    pub(crate) base_name: String,
    pub(crate) layout: MultiVolumeLayout,
    pub(crate) volume_tokens: Vec<Option<TaskDependencyToken>>,
    pub(crate) dest: PathBuf,
    pub(crate) retention: ArchiveRetention,
    pub(crate) parts: Vec<ArchivePart>,
    pub(crate) password: Option<String>,
    pub(crate) patch_options: PatchApplyOptions,
    pub(crate) expected_files: Arc<BTreeMap<String, GameFileEntry>>,
    pub(crate) prepared: Mutex<Option<PreparedArchive>>,
    pub(crate) extracted_bytes: AtomicU64,
    cache_invalid: AtomicBool,
}

impl ArchiveWork {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        base_name: String,
        layout: MultiVolumeLayout,
        volume_tokens: Vec<Option<TaskDependencyToken>>,
        dest: PathBuf,
        retention: ArchiveRetention,
        parts: Vec<ArchivePart>,
        password: Option<String>,
        patch_options: PatchApplyOptions,
        expected_files: Arc<BTreeMap<String, GameFileEntry>>,
    ) -> Result<Arc<Self>> {
        if volume_tokens.len() != layout.volume_count() {
            return Err(Error::TaskPool(format!(
                "archive {} has {} volume tokens for {} volumes",
                base_name,
                volume_tokens.len(),
                layout.volume_count()
            )));
        }
        if layout.is_remote() && retention.keeps_complete_volumes() {
            if parts.len() != layout.volume_count() {
                return Err(Error::TaskPool(format!(
                    "archive {} retains complete volumes but has {} part descriptors for {} volumes",
                    base_name,
                    parts.len(),
                    layout.volume_count()
                )));
            }
            for (index, part) in parts.iter().enumerate() {
                let path_matches = layout.path(index) == Some(part.dest.as_path());
                let size_matches = layout
                    .volume_range(index)
                    .is_some_and(|range| range.end - range.start == part.expected_size);
                if !path_matches || !size_matches {
                    return Err(Error::TaskPool(format!(
                        "archive {} part {} does not match remote volume {}",
                        base_name, part.logical_path, index
                    )));
                }
            }
        }
        Ok(Arc::new(Self {
            base_name,
            layout,
            volume_tokens,
            dest,
            retention,
            parts,
            password,
            patch_options,
            expected_files,
            prepared: Mutex::new(None),
            extracted_bytes: AtomicU64::new(0),
            cache_invalid: AtomicBool::new(false),
        }))
    }

    pub(crate) fn tokens_for_range(&self, range: std::ops::Range<u64>) -> Vec<TaskDependencyToken> {
        self.tokens_for_indices(&self.layout.volume_indices_for_range(range))
    }

    pub(crate) fn tokens_for_indices(&self, indices: &[usize]) -> Vec<TaskDependencyToken> {
        indices
            .iter()
            .filter_map(|index| self.volume_tokens.get(*index).copied().flatten())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn all_tokens(&self) -> Vec<TaskDependencyToken> {
        self.volume_tokens
            .iter()
            .copied()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn paths_for_indices(&self, indices: &[usize]) -> Vec<PathBuf> {
        indices
            .iter()
            .filter_map(|index| self.layout.path(*index).map(Path::to_path_buf))
            .collect()
    }

    pub(crate) fn should_complete_volumes(&self) -> bool {
        self.retention.keeps_complete_volumes() && self.layout.is_remote()
    }

    pub(crate) fn invalidate_range_cache(&self) {
        if self.layout.is_remote() {
            self.cache_invalid.store(true, Ordering::Release);
        }
    }

    pub(crate) fn cleanup_prepared(&self) {
        let Some(prepared) = self.prepared.lock().unwrap().take() else {
            return;
        };
        if let Err(error) = std::fs::remove_dir_all(&prepared.staging_dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %prepared.staging_dir.display(),
                    %error,
                    "failed to remove abandoned archive staging directory"
                );
            }
        }
    }
}

impl Drop for ArchiveWork {
    fn drop(&mut self) {
        if let Ok(prepared) = self.prepared.get_mut() {
            if let Some(prepared) = prepared.take() {
                if let Err(error) = std::fs::remove_dir_all(&prepared.staging_dir) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            path = %prepared.staging_dir.display(),
                            %error,
                            "failed to remove archive staging directory during work cleanup"
                        );
                    }
                }
            }
        }
        if self.cache_invalid.load(Ordering::Acquire) {
            self.layout.cleanup_cache();
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ArchiveShardExecutionState {
    active: AtomicUsize,
    failed: AtomicBool,
    failure_reported: AtomicBool,
}

impl ArchiveShardExecutionState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            failure_reported: AtomicBool::new(false),
        })
    }

    pub(crate) fn try_begin(&self) -> std::result::Result<(), bool> {
        if self.failed.load(Ordering::Acquire) {
            return Err(self.active.load(Ordering::Acquire) == 0);
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.failed.load(Ordering::Acquire) {
            let was_last = self.active.fetch_sub(1, Ordering::AcqRel) == 1;
            return Err(was_last);
        }
        Ok(())
    }

    pub(crate) fn finish(&self, succeeded: bool) -> (bool, bool) {
        let report_failure = if succeeded {
            false
        } else {
            self.failed.store(true, Ordering::Release);
            !self.failure_reported.swap(true, Ordering::AcqRel)
        };
        let was_last = self.active.fetch_sub(1, Ordering::AcqRel) == 1;
        (
            report_failure,
            was_last && self.failed.load(Ordering::Acquire),
        )
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub(crate) struct ArchiveRangeReleaseState {
    layout: MultiVolumeLayout,
    remaining: Mutex<Vec<Option<Vec<Range<u64>>>>>,
}

impl ArchiveRangeReleaseState {
    pub(crate) fn new(layout: MultiVolumeLayout, shard_ranges: Vec<Vec<Range<u64>>>) -> Arc<Self> {
        Arc::new(Self {
            layout,
            remaining: Mutex::new(shard_ranges.into_iter().map(Some).collect()),
        })
    }

    pub(crate) fn complete_shard(&self, index: usize) {
        let still_needed = {
            let mut remaining = self.remaining.lock().unwrap();
            let Some(slot) = remaining.get_mut(index) else {
                return;
            };
            if slot.take().is_none() {
                return;
            }
            remaining
                .iter()
                .filter_map(Option::as_ref)
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        };
        self.layout.prune_range_cache(&still_needed);
    }
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct ArchiveShardTask {
    pub(crate) work: Arc<ArchiveWork>,
    pub(crate) archive_index: Arc<ArchiveIndex>,
    pub(crate) staging_dir: PathBuf,
    pub(crate) entries: Vec<usize>,
    pub(crate) volume_indices: Vec<usize>,
    pub(crate) uncompressed_bytes: u64,
    pub(crate) execution_state: Arc<ArchiveShardExecutionState>,
    pub(crate) range_release: Option<(Arc<ArchiveRangeReleaseState>, usize)>,
}

pub fn archive_expected_files(
    entries: impl IntoIterator<Item = GameFileEntry>,
) -> Arc<BTreeMap<String, GameFileEntry>> {
    Arc::new(
        entries
            .into_iter()
            .map(|entry| (entry.path.replace('\\', "/").to_ascii_lowercase(), entry))
            .collect(),
    )
}
