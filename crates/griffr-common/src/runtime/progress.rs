use std::fmt;

use rapidhash::RapidHashMap as HashMap;

/// Stable work family used to group frontend-neutral progress lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressScope {
    Integrity,
    FileEnsure,
    Vfs,
    Archive,
    Predownload,
}

/// Unit-bearing phase within a progress scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressPhase {
    Verify,
    Download,
    Extract,
    Commit,
    Patch,
    Delete,
}

/// Identifies one independently rendered progress stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressLane {
    pub scope: ProgressScope,
    pub phase: ProgressPhase,
}

impl ProgressLane {
    pub const INTEGRITY_VERIFY: Self = Self::new(ProgressScope::Integrity, ProgressPhase::Verify);
    pub const INTEGRITY_DOWNLOAD: Self =
        Self::new(ProgressScope::Integrity, ProgressPhase::Download);
    pub const FILE_ENSURE_VERIFY: Self =
        Self::new(ProgressScope::FileEnsure, ProgressPhase::Verify);
    pub const FILE_ENSURE_DOWNLOAD: Self =
        Self::new(ProgressScope::FileEnsure, ProgressPhase::Download);
    pub const VFS_VERIFY: Self = Self::new(ProgressScope::Vfs, ProgressPhase::Verify);
    pub const VFS_DOWNLOAD: Self = Self::new(ProgressScope::Vfs, ProgressPhase::Download);
    pub const ARCHIVE_VERIFY: Self = Self::new(ProgressScope::Archive, ProgressPhase::Verify);
    pub const ARCHIVE_DOWNLOAD: Self = Self::new(ProgressScope::Archive, ProgressPhase::Download);
    pub const ARCHIVE_EXTRACT: Self = Self::new(ProgressScope::Archive, ProgressPhase::Extract);
    pub const ARCHIVE_COMMIT: Self = Self::new(ProgressScope::Archive, ProgressPhase::Commit);
    pub const ARCHIVE_PATCH: Self = Self::new(ProgressScope::Archive, ProgressPhase::Patch);
    pub const ARCHIVE_DELETE: Self = Self::new(ProgressScope::Archive, ProgressPhase::Delete);
    pub const PREDOWNLOAD_VERIFY: Self =
        Self::new(ProgressScope::Predownload, ProgressPhase::Verify);
    pub const PREDOWNLOAD_DOWNLOAD: Self =
        Self::new(ProgressScope::Predownload, ProgressPhase::Download);

    pub const fn new(scope: ProgressScope, phase: ProgressPhase) -> Self {
        Self { scope, phase }
    }
}

impl fmt::Display for ProgressLane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}.{:?}", self.scope, self.phase)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressUnit {
    Items,
    Bytes,
}

/// Frontend-neutral progress fact. Renderers own all mutable display state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    Started {
        lane: ProgressLane,
        unit: ProgressUnit,
        total: Option<u64>,
    },
    Advanced {
        lane: ProgressLane,
        finished: u64,
        total: Option<u64>,
        item: Option<String>,
    },
    Finished {
        lane: ProgressLane,
    },
    Failed {
        lane: ProgressLane,
        item: Option<String>,
        reason: String,
    },
}

/// Cloneable, non-failing producer handle suitable for cross-crate APIs.
#[derive(Clone, Default)]
pub struct ProgressSender {
    tx: Option<flume::Sender<ProgressUpdate>>,
}

pub struct ProgressReceiver {
    rx: flume::Receiver<ProgressUpdate>,
}

impl ProgressSender {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn channel() -> (Self, ProgressReceiver) {
        let (tx, rx) = flume::unbounded();
        (Self { tx: Some(tx) }, ProgressReceiver { rx })
    }

    pub fn emit(&self, update: ProgressUpdate) {
        if let Some(tx) = &self.tx {
            // A closed renderer must never fail the underlying work.
            let _ = tx.send(update);
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }
}

impl ProgressReceiver {
    pub fn recv(&self) -> Option<ProgressUpdate> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<ProgressUpdate> {
        self.rx.try_recv().ok()
    }

    pub async fn recv_async(&self) -> Option<ProgressUpdate> {
        self.rx.recv_async().await.ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathReuseMethod {
    Hardlink,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAttemptKind {
    Reuse(PathReuseMethod),
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOutcome {
    Pending,
    VerifiedSkipped,
    VerifiedReused {
        method: PathReuseMethod,
    },
    VerifiedDownloaded {
        bytes: u64,
    },
    Failed {
        last_attempt: Option<PathAttemptKind>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathOutcomeSummary {
    pub downloaded_files: usize,
    pub downloaded_bytes: u64,
    pub reused_files: usize,
    pub skipped_files: usize,
    pub failed_files: usize,
}

#[derive(Debug, Default, Clone)]
pub struct PathOutcomeTracker {
    outcomes: HashMap<String, PathOutcome>,
}

impl PathOutcomeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_downloaded(&mut self, path: &str, bytes: u64) {
        self.outcomes
            .insert(path.to_string(), PathOutcome::VerifiedDownloaded { bytes });
    }

    pub fn record_reused(&mut self, path: &str, method: PathReuseMethod) {
        self.outcomes
            .insert(path.to_string(), PathOutcome::VerifiedReused { method });
    }

    pub fn record_verified(&mut self, path: &str, ok: bool) {
        if ok {
            let next = match self.outcomes.get(path) {
                Some(PathOutcome::VerifiedDownloaded { bytes }) => {
                    PathOutcome::VerifiedDownloaded { bytes: *bytes }
                }
                Some(PathOutcome::VerifiedReused { method }) => {
                    PathOutcome::VerifiedReused { method: *method }
                }
                _ => PathOutcome::VerifiedSkipped,
            };
            self.outcomes.insert(path.to_string(), next);
        } else {
            self.record_failed(path);
        }
    }

    pub fn record_failed(&mut self, path: &str) {
        let last_attempt = match self.outcomes.get(path) {
            Some(PathOutcome::VerifiedDownloaded { .. }) => Some(PathAttemptKind::Download),
            Some(PathOutcome::VerifiedReused { method }) => Some(PathAttemptKind::Reuse(*method)),
            Some(PathOutcome::Failed { last_attempt }) => *last_attempt,
            _ => None,
        };
        self.outcomes
            .insert(path.to_string(), PathOutcome::Failed { last_attempt });
    }

    pub fn outcome(&self, path: &str) -> PathOutcome {
        self.outcomes
            .get(path)
            .cloned()
            .unwrap_or(PathOutcome::Pending)
    }

    pub fn summary(&self) -> PathOutcomeSummary {
        let mut summary = PathOutcomeSummary::default();
        for outcome in self.outcomes.values() {
            match outcome {
                PathOutcome::VerifiedDownloaded { bytes } => {
                    summary.downloaded_files += 1;
                    summary.downloaded_bytes = summary.downloaded_bytes.saturating_add(*bytes);
                }
                PathOutcome::VerifiedReused { .. } => {
                    summary.reused_files += 1;
                }
                PathOutcome::VerifiedSkipped => {
                    summary.skipped_files += 1;
                }
                PathOutcome::Failed { .. } => {
                    summary.failed_files += 1;
                }
                PathOutcome::Pending => {}
            }
        }
        summary
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RunningByteProgress {
    bytes_by_key: HashMap<String, u64>,
    total_bytes: u64,
}

impl RunningByteProgress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, key: &str, bytes: u64) -> u64 {
        let old_bytes = self
            .bytes_by_key
            .insert(key.to_string(), bytes)
            .unwrap_or(0);
        self.total_bytes = self
            .total_bytes
            .saturating_add(bytes)
            .saturating_sub(old_bytes);
        self.total_bytes
    }

    pub fn record_max(&mut self, key: &str, bytes: u64) -> u64 {
        let old_bytes = self.bytes_by_key.get(key).copied().unwrap_or(0);
        self.record(key, old_bytes.max(bytes))
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PathAttemptKind, PathOutcome, PathOutcomeSummary, PathOutcomeTracker, PathReuseMethod,
        ProgressLane, ProgressSender, ProgressUnit, ProgressUpdate, RunningByteProgress,
    };

    #[test]
    fn progress_channel_delivers_updates_and_closes_cleanly() {
        let lane = ProgressLane::INTEGRITY_VERIFY;
        let (sender, receiver) = ProgressSender::channel();
        sender.emit(ProgressUpdate::Started {
            lane,
            unit: ProgressUnit::Items,
            total: Some(3),
        });
        drop(sender);

        assert_eq!(
            receiver.recv(),
            Some(ProgressUpdate::Started {
                lane,
                unit: ProgressUnit::Items,
                total: Some(3),
            })
        );
        assert_eq!(receiver.recv(), None);
    }

    #[test]
    fn disabled_progress_sender_is_a_noop() {
        let sender = ProgressSender::disabled();
        assert!(!sender.is_enabled());
        sender.emit(ProgressUpdate::Finished {
            lane: ProgressLane::INTEGRITY_VERIFY,
        });
    }

    #[test]
    fn tracks_running_total_by_latest_value_per_path() {
        let mut progress = RunningByteProgress::new();

        assert_eq!(progress.record("a", 10), 10);
        assert_eq!(progress.record("a", 15), 15);
        assert_eq!(progress.record("b", 7), 22);
        assert_eq!(progress.total_bytes(), 22);
    }

    #[test]
    fn max_record_does_not_regress_on_download_retry() {
        let mut progress = RunningByteProgress::new();

        assert_eq!(progress.record_max("a", 90), 90);
        assert_eq!(progress.record_max("a", 10), 90);
        assert_eq!(progress.record_max("a", 100), 100);
    }

    #[test]
    fn outcome_tracker_classifies_final_states() {
        let mut tracker = PathOutcomeTracker::new();
        tracker.record_verified("skipped", true);
        tracker.record_reused("reused", PathReuseMethod::Hardlink);
        tracker.record_verified("reused", true);
        tracker.record_downloaded("downloaded", 42);
        tracker.record_verified("downloaded", true);
        tracker.record_failed("failed");

        assert_eq!(tracker.outcome("skipped"), PathOutcome::VerifiedSkipped);
        assert_eq!(
            tracker.outcome("reused"),
            PathOutcome::VerifiedReused {
                method: PathReuseMethod::Hardlink
            }
        );
        assert_eq!(
            tracker.outcome("downloaded"),
            PathOutcome::VerifiedDownloaded { bytes: 42 }
        );
        assert_eq!(
            tracker.outcome("failed"),
            PathOutcome::Failed { last_attempt: None }
        );
        assert_eq!(
            tracker.summary(),
            PathOutcomeSummary {
                downloaded_files: 1,
                downloaded_bytes: 42,
                reused_files: 1,
                skipped_files: 1,
                failed_files: 1,
            }
        );
    }

    #[test]
    fn failed_download_keeps_last_attempt() {
        let mut tracker = PathOutcomeTracker::new();
        tracker.record_downloaded("foo", 9);
        tracker.record_verified("foo", false);
        assert_eq!(
            tracker.outcome("foo"),
            PathOutcome::Failed {
                last_attempt: Some(PathAttemptKind::Download)
            }
        );
    }

    #[test]
    fn failed_reuse_then_download_success_overwrites_final_outcome() {
        let mut tracker = PathOutcomeTracker::new();
        tracker.record_reused("foo", PathReuseMethod::Copy);
        tracker.record_failed("foo");
        tracker.record_downloaded("foo", 7);
        tracker.record_verified("foo", true);
        assert_eq!(
            tracker.outcome("foo"),
            PathOutcome::VerifiedDownloaded { bytes: 7 }
        );
    }
}
