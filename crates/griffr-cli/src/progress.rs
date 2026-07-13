//! Progress bar implementation using indicatif.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use griffr_common::runtime::task_pool::ProgressEvent;
use griffr_common::runtime::RunningByteProgress;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Lightweight per-step progress bar.
#[derive(Clone)]
pub struct StepProgress {
    bar: ProgressBar,
    label: String,
    verbose: bool,
    started: Arc<AtomicBool>,
    multi: Option<Arc<MultiProgress>>,
    multi_index: Option<usize>,
}

impl StepProgress {
    pub fn new(label: impl Into<String>, verbose: bool) -> Self {
        let bar = ProgressBar::new(0);
        bar.set_draw_target(ProgressDrawTarget::hidden());
        Self::from_bar(bar, label, verbose, None, None)
    }

    fn new_in(
        multi: Arc<MultiProgress>,
        index: usize,
        label: impl Into<String>,
        verbose: bool,
    ) -> Self {
        let bar = ProgressBar::new(0);
        bar.set_draw_target(ProgressDrawTarget::hidden());
        Self::from_bar(bar, label, verbose, Some(multi), Some(index))
    }

    fn from_bar(
        bar: ProgressBar,
        label: impl Into<String>,
        verbose: bool,
        multi: Option<Arc<MultiProgress>>,
        multi_index: Option<usize>,
    ) -> Self {
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} {percent:>3}%")
                .unwrap()
                .progress_chars("#>-"),
        );
        Self {
            bar,
            label: label.into(),
            verbose,
            started: Arc::new(AtomicBool::new(false)),
            multi,
            multi_index,
        }
    }

    pub fn update_count(&self, completed: usize, total: usize, file: &str) {
        if total == 0 {
            return;
        }
        let completed = completed.min(total);
        self.ensure_started(total as u64);

        let should_refresh = self.verbose
            || total <= 20
            || completed <= 3
            || completed == total
            || completed.is_multiple_of(10);
        if !should_refresh {
            return;
        }

        if self.verbose && !file.is_empty() {
            self.bar
                .set_message(format!("{} {}", self.label.as_str(), file));
        } else {
            self.bar.set_message(self.label.clone());
        }
        self.bar.set_position(completed as u64);
    }

    pub fn update_bytes(&self, completed: u64, total: u64, file: &str) {
        let total = total.max(1);
        self.ensure_started(total);

        let clamped = completed.min(total);
        if self.verbose {
            self.bar.set_message(format!(
                "{} {} ({}/{})",
                self.label,
                file,
                HumanBytes(clamped),
                HumanBytes(total)
            ));
        } else {
            self.bar.set_message(format!(
                "{} {}/{}",
                self.label,
                HumanBytes(clamped),
                HumanBytes(total)
            ));
        }
        self.bar.set_position(clamped);
    }

    pub fn finish(&self) {
        if !self.started.load(Ordering::Acquire) {
            return;
        }
        let done = self.bar.position();
        let total = self.bar.length().unwrap_or(done);
        self.bar
            .finish_with_message(format!("{} done ({}/{})", self.label, done, total));
    }

    fn ensure_started(&self, total: u64) {
        if !self.started.swap(true, Ordering::AcqRel) {
            if let Some(multi) = &self.multi {
                let index = self.multi_index.expect("grouped progress index");
                let _ = multi.insert(index, self.bar.clone());
            } else {
                self.bar
                    .set_draw_target(ProgressDrawTarget::stderr_with_hz(20));
            }
            self.bar.enable_steady_tick(Duration::from_millis(120));
        }
        if self.bar.length() != Some(total) {
            self.bar.set_length(total);
        }
    }
}

/// Indeterminate progress for long operations without a cheap total.
pub struct ActivityProgress {
    bar: ProgressBar,
    label: String,
}

impl ActivityProgress {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(20));
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        bar.set_message(label.clone());
        bar.enable_steady_tick(Duration::from_millis(120));
        Self { bar, label }
    }

    pub fn finish(&self) {
        self.bar.finish_with_message(format!("{} done", self.label));
    }

    pub fn fail(&self) {
        self.bar
            .finish_with_message(format!("{} failed", self.label));
    }
}

fn grouped_progress() -> Arc<MultiProgress> {
    Arc::new(MultiProgress::with_draw_target(
        ProgressDrawTarget::stderr_with_hz(20),
    ))
}

/// Keeps count-based and byte-based progress on separate terminal rows.
pub struct CountAndByteProgress {
    count: StepProgress,
    bytes: StepProgress,
}

impl CountAndByteProgress {
    pub fn new(
        count_label: impl Into<String>,
        byte_label: impl Into<String>,
        verbose: bool,
    ) -> Self {
        let multi = grouped_progress();
        Self {
            count: StepProgress::new_in(multi.clone(), 0, count_label, verbose),
            bytes: StepProgress::new_in(multi, 1, byte_label, verbose),
        }
    }

    pub fn count_bar(&self) -> StepProgress {
        self.count.clone()
    }

    pub fn byte_bar(&self) -> StepProgress {
        self.bytes.clone()
    }

    pub fn split_callbacks(
        &self,
    ) -> (
        impl Fn(usize, usize, &str) + Clone + 'static,
        impl Fn(u64, u64, &str) + Clone + 'static,
    ) {
        let count = self.count.clone();
        let bytes = self.bytes.clone();
        (
            move |completed, total, file| count.update_count(completed, total, file),
            move |completed, total, file| bytes.update_bytes(completed, total, file),
        )
    }

    pub fn finish(&self) {
        self.count.finish();
        self.bytes.finish();
    }
}

/// Tracks bytes for files that actually entered the download path.
pub struct DownloadProgressTracker {
    bar: StepProgress,
    downloaded_by_path: RunningByteProgress,
    total_by_path: RunningByteProgress,
}

impl DownloadProgressTracker {
    pub fn new(bar: StepProgress) -> Self {
        Self {
            bar,
            downloaded_by_path: RunningByteProgress::new(),
            total_by_path: RunningByteProgress::new(),
        }
    }

    pub fn handle_event(&mut self, event: &ProgressEvent) {
        let path = match event {
            ProgressEvent::DownloadStarted { path, total_bytes } => {
                self.total_by_path.record(path, *total_bytes);
                path
            }
            ProgressEvent::DownloadedBytes {
                path, total_bytes, ..
            } => {
                self.downloaded_by_path
                    .handle_download_event(event)
                    .expect("download event");
                self.total_by_path.record(path, *total_bytes);
                path
            }
            ProgressEvent::Downloaded { path, bytes } => {
                self.downloaded_by_path
                    .handle_download_event(event)
                    .expect("download event");
                self.total_by_path.record(path, *bytes);
                path
            }
            _ => return,
        };
        self.bar.update_bytes(
            self.downloaded_by_path.total_bytes(),
            self.total_by_path.total_bytes(),
            path,
        );
    }
}

/// Tracks aggregate extraction bytes independently from archive downloads.
pub struct ExtractionProgressTracker {
    bar: StepProgress,
    extracted_by_archive: RunningByteProgress,
    total_by_archive: RunningByteProgress,
}

impl ExtractionProgressTracker {
    pub fn new(bar: StepProgress) -> Self {
        Self {
            bar,
            extracted_by_archive: RunningByteProgress::new(),
            total_by_archive: RunningByteProgress::new(),
        }
    }

    pub fn handle_event(&mut self, event: &ProgressEvent) {
        let ProgressEvent::ExtractedBytes {
            path,
            bytes,
            total_bytes,
        } = event
        else {
            return;
        };
        self.extracted_by_archive.record(path, *bytes);
        self.total_by_archive.record(path, *total_bytes);
        self.bar.update_bytes(
            self.extracted_by_archive.total_bytes(),
            self.total_by_archive.total_bytes(),
            path,
        );
    }
}

/// Keeps archive verification, network transfer, and extraction on stable rows.
pub struct ArchivePipelineProgress {
    part_count: StepProgress,
    download: StepProgress,
    extract: StepProgress,
    commit: StepProgress,
    patch: StepProgress,
    delete: StepProgress,
    total_parts: usize,
    verified_parts: usize,
    download_tracker: DownloadProgressTracker,
    extraction_tracker: ExtractionProgressTracker,
}

impl ArchivePipelineProgress {
    pub fn new(label: &str, total_parts: usize, verbose: bool) -> Self {
        let multi = grouped_progress();
        let part_count =
            StepProgress::new_in(multi.clone(), 0, format!("{label}.archive-verify"), verbose);
        let download = StepProgress::new_in(
            multi.clone(),
            1,
            format!("{label}.archive-download"),
            verbose,
        );
        let extract = StepProgress::new_in(
            multi.clone(),
            2,
            format!("{label}.archive-extract"),
            verbose,
        );
        let commit =
            StepProgress::new_in(multi.clone(), 3, format!("{label}.archive-commit"), verbose);
        let patch =
            StepProgress::new_in(multi.clone(), 4, format!("{label}.archive-patch"), verbose);
        let delete = StepProgress::new_in(multi, 5, format!("{label}.archive-delete"), verbose);
        part_count.update_count(0, total_parts, "");
        Self {
            part_count: part_count.clone(),
            download: download.clone(),
            extract: extract.clone(),
            commit,
            patch,
            delete,
            total_parts,
            verified_parts: 0,
            download_tracker: DownloadProgressTracker::new(download),
            extraction_tracker: ExtractionProgressTracker::new(extract),
        }
    }

    pub fn handle_event(&mut self, event: &ProgressEvent) {
        if let ProgressEvent::Verified { path, .. } = event {
            self.verified_parts = self.verified_parts.saturating_add(1);
            self.part_count
                .update_count(self.verified_parts, self.total_parts, path);
        }
        match event {
            ProgressEvent::ArchiveCommitProgress {
                path,
                completed,
                total,
            } => self.commit.update_count(*completed, *total, path),
            ProgressEvent::PatchProgress {
                path,
                completed,
                total,
            } => self.patch.update_count(*completed, *total, path),
            ProgressEvent::DeleteProgress {
                path,
                completed,
                total,
            } => self.delete.update_count(*completed, *total, path),
            _ => {}
        }
        self.download_tracker.handle_event(event);
        self.extraction_tracker.handle_event(event);
    }

    pub fn finish(&self) {
        self.part_count.finish();
        self.download.finish();
        self.extract.finish();
        self.commit.finish();
        self.patch.finish();
        self.delete.finish();
    }
}

/// Tracks verify-count progress across a fixed task batch.
pub struct VerifyTaskProgressTracker {
    bar: StepProgress,
    total_tasks: usize,
    finished_tasks: usize,
}

impl VerifyTaskProgressTracker {
    pub fn new(bar: StepProgress, total_tasks: usize) -> Self {
        bar.update_count(0, total_tasks, "");
        Self {
            bar,
            total_tasks,
            finished_tasks: 0,
        }
    }

    pub fn handle_event(&mut self, event: &ProgressEvent) {
        if let ProgressEvent::Verified { path, .. } = event {
            self.finished_tasks = self.finished_tasks.saturating_add(1);
            self.bar
                .update_count(self.finished_tasks, self.total_tasks, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hide_group(bar: &StepProgress) {
        bar.multi
            .as_ref()
            .expect("grouped progress")
            .set_draw_target(ProgressDrawTarget::hidden());
    }

    #[test]
    fn count_and_byte_progress_keep_independent_units() {
        let progress = CountAndByteProgress::new("verify", "repair.download", false);
        hide_group(&progress.count);

        let (count, bytes) = progress.split_callbacks();
        count(1, 10, "a.bin");
        let count_position = progress.count.bar.position();
        let count_length = progress.count.bar.length();

        bytes(64, 128, "b.bin");

        assert_eq!(progress.count.bar.position(), count_position);
        assert_eq!(progress.count.bar.length(), count_length);
        assert_eq!(progress.bytes.bar.position(), 64);
        assert_eq!(progress.bytes.bar.length(), Some(128));
    }

    #[test]
    fn download_progress_counts_only_started_downloads() {
        let multi = grouped_progress();
        multi.set_draw_target(ProgressDrawTarget::hidden());
        let bar = StepProgress::new_in(multi, 0, "download", false);
        let mut tracker = DownloadProgressTracker::new(bar.clone());

        tracker.handle_event(&ProgressEvent::DownloadStarted {
            path: "a.bin".to_string(),
            total_bytes: 100,
        });
        tracker.handle_event(&ProgressEvent::DownloadedBytes {
            path: "a.bin".to_string(),
            bytes: 40,
            total_bytes: 100,
        });
        tracker.handle_event(&ProgressEvent::DownloadStarted {
            path: "b.bin".to_string(),
            total_bytes: 20,
        });

        assert_eq!(bar.bar.position(), 40);
        assert_eq!(bar.bar.length(), Some(120));
    }

    #[test]
    fn archive_pipeline_keeps_download_and_extract_separate() {
        let mut progress = ArchivePipelineProgress::new("install", 1, false);
        hide_group(&progress.part_count);

        progress.handle_event(&ProgressEvent::DownloadStarted {
            path: "pack.001".to_string(),
            total_bytes: 100,
        });
        progress.handle_event(&ProgressEvent::DownloadedBytes {
            path: "pack.001".to_string(),
            bytes: 50,
            total_bytes: 100,
        });
        progress.handle_event(&ProgressEvent::ExtractedBytes {
            path: "pack".to_string(),
            bytes: 20,
            total_bytes: 200,
        });

        assert_eq!(progress.download.bar.position(), 50);
        assert_eq!(progress.download.bar.length(), Some(100));
        assert_eq!(progress.extract.bar.position(), 20);
        assert_eq!(progress.extract.bar.length(), Some(200));
    }
}
