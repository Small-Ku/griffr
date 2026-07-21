use crate::api::types::GameFileEntry;
use crate::download::extractor::{ArchiveIndex, MultiVolumeLayout};
use crate::error::{Error, Result};
use crate::runtime::{
    PatchApplyOptions, PatchArtifactProbe, PatchCheckReport, PatchPlan, PatchProbePlan,
    PlannedPatchEntry,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::runtime::task_pool::fs_ops::CommitFileBatch;
use crate::runtime::task_pool::graph::TaskDependencyToken;
use crate::runtime::task_pool::verify::VerifiedArtifactCache;

use super::tasks::{ArchivePart, ArchiveRetention};

#[doc(hidden)]
#[derive(Debug)]
pub struct PatchApplyWork {
    plan: Arc<PatchPlan>,
    finished_entries: AtomicUsize,
    verification_cache: VerifiedArtifactCache,
}

impl PatchApplyWork {
    pub(crate) fn new(plan: PatchPlan) -> Arc<Self> {
        Arc::new(Self {
            plan: Arc::new(plan),
            finished_entries: AtomicUsize::new(0),
            verification_cache: VerifiedArtifactCache::default(),
        })
    }

    pub(crate) fn plan(&self) -> &PatchPlan {
        &self.plan
    }

    pub(crate) fn entry(&self, index: usize) -> Option<&PlannedPatchEntry> {
        self.plan.entries.get(index)
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.plan.entries.len()
    }

    pub(crate) fn finish_entry(&self) -> usize {
        self.finished_entries.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn verification_cache(&self) -> &VerifiedArtifactCache {
        &self.verification_cache
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PatchCheckWork {
    probes: Vec<PatchArtifactProbe>,
    relocation_root: Option<PathBuf>,
    relocation_bytes: Mutex<Option<std::result::Result<u64, String>>>,
    verification_cache: VerifiedArtifactCache,
}

impl PatchCheckWork {
    pub(crate) fn new(plan: PatchProbePlan) -> Arc<Self> {
        Arc::new(Self {
            probes: plan.artifacts,
            relocation_root: plan.relocation_root,
            relocation_bytes: Mutex::new(None),
            verification_cache: VerifiedArtifactCache::default(),
        })
    }

    pub(crate) fn probe_count(&self) -> usize {
        self.probes.len()
    }

    pub(crate) fn probe_path(&self, index: usize) -> Option<&Path> {
        self.probes.get(index).map(|probe| probe.path.as_path())
    }

    pub(crate) fn probe_size(&self, index: usize) -> Option<u64> {
        self.probes.get(index).map(|probe| probe.expected_size)
    }

    pub(crate) fn relocation_root(&self) -> Option<&Path> {
        self.relocation_root.as_deref()
    }

    pub(crate) fn run_probe(&self, index: usize) -> Result<()> {
        let probe = self.probes.get(index).ok_or_else(|| Error::Message {
            context: "Task pool error: ",
            detail: format!("patch probe index {index} is out of range"),
        })?;
        let _ = self.verification_cache.build_issue(
            &probe.path,
            &probe.logical_path,
            &probe.expected_md5,
            Some(probe.expected_size),
        );
        Ok(())
    }

    pub(crate) fn measure_relocation(&self) -> Result<()> {
        let result = self
            .relocation_root
            .as_deref()
            .map(directory_size_sync)
            .transpose()
            .map(|size| size.unwrap_or(0))
            .map_err(|error| error.to_string());
        *self.relocation_bytes.lock().unwrap() = Some(result.clone());
        result.map(|_| ()).map_err(|detail| Error::Message {
            context: "Task pool error: ",
            detail,
        })
    }

    pub(crate) fn measured_relocation_bytes(&self) -> Result<Option<u64>> {
        if self.relocation_root.is_none() {
            return Ok(None);
        }
        match self.relocation_bytes.lock().unwrap().as_ref() {
            Some(Ok(bytes)) => Ok(Some(*bytes)),
            Some(Err(error)) => Err(Error::Message {
                context: "Task pool error: ",
                detail: error.clone(),
            }),
            None => Err(Error::Message {
                context: "Task pool error: ",
                detail: "patch relocation scan did not finish before the plan was saved"
                    .to_string(),
            }),
        }
    }

    pub(crate) fn verification_cache(&self) -> &VerifiedArtifactCache {
        &self.verification_cache
    }
}

fn directory_size_sync(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|source| Error::IoAt {
            action: "read directory",
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::IoAt {
                action: "read directory",
                path: directory.clone(),
                source,
            })?;
            let entry_path = entry.path();
            let metadata =
                std::fs::symlink_metadata(&entry_path).map_err(|source| Error::IoAt {
                    action: "query file metadata for",
                    path: entry_path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ArchiveCommitWork {
    pub(crate) archive: Arc<ArchiveWork>,
    pub(crate) staging_dir: PathBuf,
    pub(crate) batches: Vec<CommitFileBatch>,
    finished_files: AtomicUsize,
    total_files: usize,
}

impl ArchiveCommitWork {
    pub(crate) fn new(
        archive: Arc<ArchiveWork>,
        staging_dir: PathBuf,
        batches: Vec<CommitFileBatch>,
    ) -> Arc<Self> {
        let total_files = batches.iter().map(|batch| batch.jobs.len()).sum();
        Arc::new(Self {
            archive,
            staging_dir,
            batches,
            finished_files: AtomicUsize::new(0),
            total_files,
        })
    }

    pub(crate) fn batch(&self, index: usize) -> Option<&CommitFileBatch> {
        self.batches.get(index)
    }

    pub(crate) fn batch_count(&self) -> usize {
        self.batches.len()
    }

    pub(crate) fn finish_file(&self) -> usize {
        self.finished_files.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn total_files(&self) -> usize {
        self.total_files
    }
}

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
    pub(crate) excluded_commit_paths: Arc<std::collections::BTreeSet<String>>,
    pub(crate) prepared: Mutex<Option<PreparedArchive>>,
    pub(crate) extracted_bytes: AtomicU64,
    saved_volumes: Mutex<Vec<bool>>,
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
        excluded_commit_paths: Arc<std::collections::BTreeSet<String>>,
    ) -> Result<Arc<Self>> {
        if volume_tokens.len() != layout.volume_count() {
            return Err(Error::Message {
                context: "Task pool error: ",
                detail: format!(
                    "archive {} has {} volume tokens for {} volumes",
                    base_name,
                    volume_tokens.len(),
                    layout.volume_count()
                ),
            });
        }
        if layout.is_remote() && retention.keeps_full_volumes() {
            if parts.len() != layout.volume_count() {
                return Err(Error::Message {
                    context: "Task pool error: ",
                    detail: format!(
                    "archive {} retains full volumes but has {} part descriptors for {} volumes",
                    base_name,
                    parts.len(),
                    layout.volume_count()
                ),
                });
            }
            for (index, part) in parts.iter().enumerate() {
                let path_matches = layout.path(index) == Some(part.dest.as_path());
                let size_matches = layout
                    .volume_range(index)
                    .is_some_and(|range| range.end - range.start == part.expected_size);
                if !path_matches || !size_matches {
                    return Err(Error::Message {
                        context: "Task pool error: ",
                        detail: format!(
                            "archive {} part {} does not match remote volume {}",
                            base_name, part.logical_path, index
                        ),
                    });
                }
            }
        }
        let volume_count = layout.volume_count();
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
            excluded_commit_paths,
            prepared: Mutex::new(None),
            extracted_bytes: AtomicU64::new(0),
            saved_volumes: Mutex::new(vec![false; volume_count]),
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

    pub(crate) fn should_save_full_volumes(&self) -> bool {
        self.retention.keeps_full_volumes() && self.layout.is_remote()
    }

    pub(crate) fn mark_volume_saved(&self, index: usize) {
        let still_needed = {
            let mut saved = self.saved_volumes.lock().unwrap();
            let Some(slot) = saved.get_mut(index) else {
                return;
            };
            *slot = true;
            saved
                .iter()
                .enumerate()
                .filter_map(|(volume_index, done)| {
                    (!*done)
                        .then(|| self.layout.volume_range(volume_index))
                        .flatten()
                })
                .collect::<Vec<_>>()
        };
        self.layout.prune_range_cache(&still_needed);
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
pub struct ArchiveShardRunState {
    active: AtomicUsize,
    failed: AtomicBool,
    failure_reported: AtomicBool,
}

impl ArchiveShardRunState {
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

    pub(crate) fn finish_shard(&self, index: usize) {
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
    pub(crate) estimated_cost: u64,
    pub(crate) run_state: Arc<ArchiveShardRunState>,
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
