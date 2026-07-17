use crate::runtime::PatchApplyOptions;
use md5::Md5;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Selects the download throttle. Local verification and reuse never use the
/// VFS CDN queue, even when a later fallback download is VFS-classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferClass {
    General,
    Vfs,
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
pub struct ArchiveInstallGroup {
    remaining: AtomicUsize,
    failed: AtomicBool,
    continuation: Mutex<Option<Task>>,
}

impl ArchiveInstallGroup {
    pub(crate) fn new(part_count: usize, continuation: Task) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(part_count),
            failed: AtomicBool::new(false),
            continuation: Mutex::new(Some(continuation)),
        })
    }

    pub(crate) fn finish_part(&self, succeeded: bool, spawned: &mut Vec<Task>) {
        if !succeeded {
            self.failed.store(true, Ordering::Release);
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if self.failed.load(Ordering::Acquire) {
            self.continuation.lock().unwrap().take();
            return;
        }
        if let Some(task) = self.continuation.lock().unwrap().take() {
            spawned.push(task);
        }
    }
}

#[derive(Debug)]
pub struct ReuseCandidateGroup {
    remaining: AtomicUsize,
    verified_sources: Mutex<Vec<(bool, usize, PathBuf)>>,
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    download_url: Option<String>,
    allow_copy_fallback: bool,
    retry_count: u32,
    transfer_class: TransferClass,
}

impl ReuseCandidateGroup {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        group_count: usize,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    ) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(group_count),
            verified_sources: Mutex::new(Vec::new()),
            dest,
            logical_path,
            expected_md5,
            expected_size,
            download_url,
            allow_copy_fallback,
            retry_count,
            transfer_class,
        })
    }

    pub(crate) fn finish_volume(
        &self,
        group_index: usize,
        copy_only: bool,
        source: Option<PathBuf>,
        spawned: &mut Vec<Task>,
        event_tx: &flume::Sender<WorkerEvent>,
    ) {
        if let Some(source) = source {
            self.verified_sources
                .lock()
                .unwrap()
                .push((copy_only, group_index, source));
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }

        let mut guard = self.verified_sources.lock().unwrap();
        let mut sources = std::mem::take(&mut *guard);
        drop(guard);
        sources.sort_by_key(|(copy_only, index, _)| (*copy_only, *index));
        let mut sources = sources
            .into_iter()
            .map(|(copy_only, _, source)| (copy_only, source))
            .collect::<Vec<_>>();
        if !sources.is_empty() {
            let (copy_only, source) = sources.remove(0);
            spawned.push(Task::ReuseFile {
                source,
                copy_only,
                remaining_source_candidates: sources,
                dest: self.dest.clone(),
                logical_path: self.logical_path.clone(),
                expected_md5: self.expected_md5.clone(),
                expected_size: self.expected_size,
                download_url: self.download_url.clone(),
                allow_copy_fallback: self.allow_copy_fallback,
                retry_count: self.retry_count,
                transfer_class: self.transfer_class,
            });
        } else if let Some(url) = self.download_url.clone() {
            spawned.push(Task::Download {
                url,
                dest: self.dest.clone(),
                logical_path: self.logical_path.clone(),
                expected_md5: self.expected_md5.clone(),
                expected_size: Some(self.expected_size),
                retry_count: self.retry_count,
                transfer_class: self.transfer_class,
            });
        } else {
            let _ = event_tx.send(WorkerEvent::Failed {
                path: self.logical_path.clone(),
                reason: "no usable source candidates".to_string(),
            });
        }
    }
}

#[derive(Debug, Clone)]
pub enum Task {
    InstallArchive {
        base_name: String,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
        patch_options: PatchApplyOptions,
        parts: Vec<ArchivePart>,
    },
    InstallArchivePart {
        part: ArchivePart,
        group: Arc<ArchiveInstallGroup>,
        retry_count: u32,
    },
    TransferArchivePart {
        part: ArchivePart,
        group: Arc<ArchiveInstallGroup>,
        retry_count: u32,
        resume: DownloadResumeState,
    },
    /// CPU-side partial-file inspection and prefix hashing. This task creates
    /// `TransferDownload` only after the resume state is ready.
    Download {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    #[doc(hidden)]
    TransferDownload {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
        transfer_class: TransferClass,
        resume: DownloadResumeState,
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
        retry_count: u32,
        transfer_class: TransferClass,
    },
    VerifyReuseVolume {
        group_index: usize,
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
        remaining_source_candidates: Vec<(bool, PathBuf)>,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        retry_count: u32,
        transfer_class: TransferClass,
    },
    Extract {
        base_name: String,
        volumes: Vec<PathBuf>,
        dest: PathBuf,
        cleanup: bool,
        password: Option<String>,
        patch_options: PatchApplyOptions,
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
    /// Builds a CPU-first verify/repair graph. Only explicit relink mode skips
    /// destination verification because relinking is itself the requested work.
    pub fn ensure_file(spec: FileEnsureTask) -> Self {
        let repair = Self::RepairFile {
            dest: spec.dest.clone(),
            logical_path: spec.logical_path.clone(),
            expected_md5: spec.expected_md5.clone(),
            expected_size: spec.expected_size,
            source_candidates: spec.source_candidates,
            download_url: spec.download_url,
            allow_copy_fallback: spec.allow_copy_fallback,
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

#[derive(Debug, Clone)]
pub struct ArchivePart {
    pub sequence: u64,
    pub url: String,
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
}

mod config;
mod events;
mod progress;

pub use config::{TaskPoolConfig, DEFAULT_PROGRESS_BUFFER_BYTES};
pub(crate) use events::WorkerEvent;
pub use events::{TaskOutcome, TaskPoolResult, TaskPoolRunner};
pub use progress::TaskProgress;

#[cfg(test)]
mod tests;
