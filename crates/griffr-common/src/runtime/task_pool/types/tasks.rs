use crate::api::types::GameFileEntry;
use crate::download::extractor::{ArchiveDirectory, ArchiveIndex, ArchiveRangeRequest};
use crate::error::{Error, Result};
use crate::runtime::PatchApplyOptions;
use md5::Md5;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::archive::{
    ArchiveCommitWork, ArchiveShardTask, ArchiveWork, PatchApplyWork, PatchCheckWork,
};

/// Selects the download throttle. Local verification and reuse never use the
/// VFS CDN queue, even when a later fallback download is VFS-classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferClass {
    General,
    Vfs,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRangePriority {
    ExtractionCritical,
    RetentionBackground,
}

/// Prepared incremental-MD5 state passed from a CPU preparation task to the
/// network transfer task. The hasher is intentionally opaque to callers.
#[doc(hidden)]
#[derive(Clone)]
pub struct DownloadResumeState {
    pub(crate) offset: u64,
    hasher: Arc<Mutex<Option<Md5>>>,
}

impl DownloadResumeState {
    pub(crate) fn new(offset: u64, hasher: Md5) -> Self {
        Self {
            offset,
            hasher: Arc::new(Mutex::new(Some(hasher))),
        }
    }

    pub(crate) fn take_hasher(self) -> Md5 {
        let mut hasher = self.hasher.lock().unwrap();
        hasher
            .take()
            .expect("download resume state consumed more than once")
    }
}

impl std::fmt::Debug for DownloadResumeState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DownloadResumeState")
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct FileEnsureTask {
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
    pub source_candidates: Vec<PathBuf>,
    pub download_url: Option<String>,
    pub allow_copy_fallback: bool,
    pub prefer_reuse: bool,
    pub retry_count: u32,
    pub transfer_class: TransferClass,
}

#[derive(Debug)]
pub struct ReuseCandidateGroup {
    remaining: AtomicUsize,
    resolved: AtomicBool,
    copy_phase_groups: Mutex<Option<Vec<Vec<PathBuf>>>>,
    all_sources: Vec<PathBuf>,
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    download_url: Option<String>,
    allow_copy_fallback: bool,
    verify_destination_fallback: bool,
    retry_count: u32,
    transfer_class: TransferClass,
}

impl ReuseCandidateGroup {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        group_count: usize,
        copy_phase_groups: Vec<Vec<PathBuf>>,
        all_sources: Vec<PathBuf>,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        verify_destination_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    ) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(group_count),
            resolved: AtomicBool::new(false),
            copy_phase_groups: Mutex::new(
                (!copy_phase_groups.is_empty()).then_some(copy_phase_groups),
            ),
            all_sources,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            download_url,
            allow_copy_fallback,
            verify_destination_fallback,
            retry_count,
            transfer_class,
        })
    }

    pub(crate) fn is_resolved(&self) -> bool {
        self.resolved.load(Ordering::Acquire)
    }

    pub(crate) fn finish_volume(
        &self,
        copy_only: bool,
        source: Option<PathBuf>,
    ) -> Result<Vec<Task>> {
        if let Some(source) = source {
            if self
                .resolved
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Ok(Vec::new());
            }
            let remaining_source_candidates = self
                .all_sources
                .iter()
                .filter(|candidate| *candidate != &source)
                .cloned()
                .collect();
            return Ok(vec![Task::ReuseFile {
                source,
                copy_only,
                remaining_source_candidates,
                dest: self.dest.clone(),
                logical_path: self.logical_path.clone(),
                expected_md5: self.expected_md5.clone(),
                expected_size: self.expected_size,
                download_url: self.download_url.clone(),
                allow_copy_fallback: self.allow_copy_fallback,
                verify_destination_fallback: self.verify_destination_fallback,
                retry_count: self.retry_count,
                transfer_class: self.transfer_class,
            }]);
        }

        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1
            || self
                .resolved
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Ok(Vec::new());
        }

        if let Some(copy_groups) = self.copy_phase_groups.lock().unwrap().take() {
            let group = Self::new(
                copy_groups.len(),
                Vec::new(),
                self.all_sources.clone(),
                self.dest.clone(),
                self.logical_path.clone(),
                self.expected_md5.clone(),
                self.expected_size,
                self.download_url.clone(),
                self.allow_copy_fallback,
                self.verify_destination_fallback,
                self.retry_count,
                self.transfer_class,
            );
            return Ok(copy_groups
                .into_iter()
                .map(|candidates| Task::VerifyReuseVolume {
                    copy_only: true,
                    candidates,
                    logical_path: self.logical_path.clone(),
                    expected_md5: self.expected_md5.clone(),
                    expected_size: self.expected_size,
                    group: group.clone(),
                })
                .collect());
        }

        destination_or_download_tasks(
            self.dest.clone(),
            self.logical_path.clone(),
            self.expected_md5.clone(),
            self.expected_size,
            self.download_url.clone(),
            self.verify_destination_fallback,
            self.retry_count,
            self.transfer_class,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn destination_or_download_tasks(
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    download_url: Option<String>,
    verify_destination_fallback: bool,
    retry_count: u32,
    transfer_class: TransferClass,
) -> Result<Vec<Task>> {
    let download = download_url.map(|url| Task::Download {
        url,
        dest: dest.clone(),
        logical_path: logical_path.clone(),
        expected_md5: expected_md5.clone(),
        expected_size: Some(expected_size),
        retry_count,
        transfer_class,
        resume: None,
    });
    if verify_destination_fallback {
        return Ok(vec![Task::Verify {
            path: dest,
            logical_path,
            expected_md5,
            expected_size: Some(expected_size),
            on_fail: download.map(Box::new),
        }]);
    }
    if let Some(download) = download {
        return Ok(vec![download]);
    }
    Err(Error::Message {
        context: "Task pool error: ",
        detail: format!("no usable source candidates for {logical_path}"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRetention {
    Ephemeral,
    KeepFullVolumes,
}

impl ArchiveRetention {
    pub const fn from_keep_full_volumes(keep: bool) -> Self {
        if keep {
            Self::KeepFullVolumes
        } else {
            Self::Ephemeral
        }
    }

    pub const fn keeps_full_volumes(self) -> bool {
        matches!(self, Self::KeepFullVolumes)
    }
}

#[derive(Debug, Clone)]
pub struct ArchivePart {
    pub sequence: u64,
    pub url: String,
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
}

/// Selects the backing data for one archive flow. Both sources use the same
/// directory scan, index read, extraction, commit, and cleanup DAG.
#[derive(Debug, Clone)]
pub enum ArchiveSource {
    Remote(Vec<ArchivePart>),
    Local(Vec<PathBuf>),
}

#[derive(Debug, Clone)]
pub enum Task {
    OpenArchive {
        base_name: String,
        source: ArchiveSource,
        dest: PathBuf,
        retention: ArchiveRetention,
        password: Option<String>,
        patch_options: PatchApplyOptions,
        expected_files: Arc<BTreeMap<String, GameFileEntry>>,
        excluded_commit_paths: Arc<BTreeSet<String>>,
    },
    /// A download changes from CPU preparation to async transfer when `resume`
    /// becomes `Some`; both stages retain this same canonical task payload.
    Download {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
        transfer_class: TransferClass,
        #[doc(hidden)]
        resume: Option<DownloadResumeState>,
    },
    Verify {
        path: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        on_fail: Option<Box<Task>>,
    },
    RepairFile {
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        source_candidates: Vec<PathBuf>,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        verify_destination_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    VerifyReuseVolume {
        copy_only: bool,
        candidates: Vec<PathBuf>,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        group: Arc<ReuseCandidateGroup>,
    },
    ReuseFile {
        source: PathBuf,
        copy_only: bool,
        remaining_source_candidates: Vec<PathBuf>,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        verify_destination_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    #[doc(hidden)]
    FetchArchiveRange {
        work: Arc<ArchiveWork>,
        request: ArchiveRangeRequest,
        retry_count: u32,
        priority: ArchiveRangePriority,
    },
    #[doc(hidden)]
    DiscoverArchiveDirectory {
        work: Arc<ArchiveWork>,
        required_range: Option<std::ops::Range<u64>>,
    },
    #[doc(hidden)]
    InspectArchiveIndex {
        work: Arc<ArchiveWork>,
        directory: ArchiveDirectory,
    },
    #[doc(hidden)]
    ReadArchiveControls {
        work: Arc<ArchiveWork>,
        archive_index: Arc<ArchiveIndex>,
    },
    #[doc(hidden)]
    ProbePatchArtifact {
        patch_check: Arc<PatchCheckWork>,
        probe_index: usize,
    },
    #[doc(hidden)]
    MeasurePatchRelocation {
        patch_check: Arc<PatchCheckWork>,
    },
    #[doc(hidden)]
    SavePatchPlan {
        work: Arc<ArchiveWork>,
        archive_index: Arc<ArchiveIndex>,
        patch_check: Arc<PatchCheckWork>,
    },
    #[doc(hidden)]
    ExtractArchiveShard {
        shard: ArchiveShardTask,
    },
    #[doc(hidden)]
    RetainArchiveVolume {
        work: Arc<ArchiveWork>,
        volume_index: usize,
    },
    #[doc(hidden)]
    CommitArchive {
        work: Arc<ArchiveWork>,
    },
    #[doc(hidden)]
    CommitArchiveBatch {
        commit: Arc<ArchiveCommitWork>,
        batch_index: usize,
    },
    #[doc(hidden)]
    FinishArchiveCommit {
        commit: Arc<ArchiveCommitWork>,
    },
    #[doc(hidden)]
    PreparePatchApply {
        patch: Arc<PatchApplyWork>,
    },
    #[doc(hidden)]
    ApplyPatchEntry {
        patch: Arc<PatchApplyWork>,
        entry_index: usize,
    },
    #[doc(hidden)]
    ReleasePatchBase {
        patch: Arc<PatchApplyWork>,
        base: PathBuf,
    },
    #[doc(hidden)]
    ApplyPatchDeletes {
        patch: Arc<PatchApplyWork>,
    },
    #[doc(hidden)]
    CommitPatchDeferred {
        patch: Arc<PatchApplyWork>,
    },
    #[doc(hidden)]
    CleanPatchApply {
        patch: Arc<PatchApplyWork>,
        archive: Arc<ArchiveWork>,
    },
    #[doc(hidden)]
    CleanupArchive {
        work: Arc<ArchiveWork>,
    },
    ApplyExtractedVfsPatchManifest {
        install_root: PathBuf,
    },
    ApplyDeleteManifest {
        install_root: PathBuf,
    },
    Hardlink {
        src: PathBuf,
        dest: PathBuf,
    },
}

impl Task {
    /// Returns the concrete destination inspected or changed by a file task.
    /// Composite archive tasks intentionally return `None`; callers must use
    /// their archive manifest when assigning path ownership.
    pub fn target_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Download { dest, .. }
            | Self::RepairFile { dest, .. }
            | Self::ReuseFile { dest, .. } => Some(dest.as_path()),
            Self::Verify { path, .. } => Some(path.as_path()),
            _ => None,
        }
    }

    /// Returns the user-facing logical path for a file task.
    pub fn logical_path(&self) -> Option<&str> {
        match self {
            Self::Download { logical_path, .. }
            | Self::Verify { logical_path, .. }
            | Self::RepairFile { logical_path, .. }
            | Self::VerifyReuseVolume { logical_path, .. }
            | Self::ReuseFile { logical_path, .. } => Some(logical_path.as_str()),
            _ => None,
        }
    }

    /// Builds a CPU-first verify/repair graph. Explicit relink mode probes reuse
    /// first, then verifies the destination before allowing a network fallback.
    pub fn ensure_file(spec: FileEnsureTask) -> Self {
        let repair = Self::RepairFile {
            dest: spec.dest.clone(),
            logical_path: spec.logical_path.clone(),
            expected_md5: spec.expected_md5.clone(),
            expected_size: spec.expected_size,
            source_candidates: spec.source_candidates,
            download_url: spec.download_url,
            allow_copy_fallback: spec.allow_copy_fallback,
            verify_destination_fallback: spec.prefer_reuse,
            retry_count: spec.retry_count,
            transfer_class: spec.transfer_class,
        };
        if spec.prefer_reuse {
            repair
        } else {
            Self::Verify {
                path: spec.dest,
                logical_path: spec.logical_path,
                expected_md5: spec.expected_md5,
                expected_size: Some(spec.expected_size),
                on_fail: Some(Box::new(repair)),
            }
        }
    }
}
