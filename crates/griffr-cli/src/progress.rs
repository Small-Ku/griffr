//! Progress bar implementation using indicatif

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use griffr_common::runtime::RunningByteProgress;
use indicatif::{HumanBytes, ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Lightweight per-step progress bar (verify/repair/materialize).
#[derive(Clone)]
pub struct StepProgress {
    bar: ProgressBar,
    label: String,
    verbose: bool,
    started: Arc<AtomicBool>,
}

impl StepProgress {
    pub fn new(label: impl Into<String>, verbose: bool) -> Self {
        let bar = ProgressBar::new(0);
        bar.set_draw_target(ProgressDrawTarget::hidden());
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
        }
    }

    pub fn update(&self, current: usize, total: usize, file: &str) {
        let current = current.saturating_add(1);
        let total = total.max(1);
        let total_u64 = total as u64;
        self.ensure_started(total_u64);

        let should_refresh =
            self.verbose || total <= 20 || current <= 3 || current == total || (current % 10 == 0);
        if !should_refresh {
            return;
        }

        if self.verbose {
            self.bar
                .set_message(format!("{} {}", self.label.as_str(), file));
        } else {
            self.bar.set_message(self.label.clone());
        }
        self.bar.set_position(current as u64);
    }

    pub fn update_bytes(&self, downloaded: u64, total: u64, file: &str) {
        let total = total.max(1);
        self.ensure_started(total);

        let clamped = downloaded.min(total);
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

    pub fn split_callbacks(
        &self,
    ) -> (
        impl Fn(usize, usize, &str) + Clone + 'static,
        impl Fn(u64, u64, &str) + Clone + 'static,
    ) {
        let progress_bar = self.clone();
        let download_bar = self.clone();
        (
            move |current, total, file| progress_bar.update(current, total, file),
            move |downloaded, total, file| download_bar.update_bytes(downloaded, total, file),
        )
    }

    pub fn byte_callback(&self, file: &'static str) -> impl Fn(u64, u64) + Clone + 'static {
        let bar = self.clone();
        move |downloaded, total| bar.update_bytes(downloaded, total, file)
    }

    fn ensure_started(&self, total: u64) {
        if !self.started.swap(true, Ordering::AcqRel) {
            self.bar
                .set_draw_target(ProgressDrawTarget::stderr_with_hz(20));
            self.bar.enable_steady_tick(Duration::from_millis(120));
        }
        if self.bar.length() != Some(total) {
            self.bar.set_length(total);
        }
    }
}

/// Tracks aggregate download/extraction progress across multiple task-pool files.
pub struct ByteProgressTracker {
    bar: StepProgress,
    base_total_bytes: u64,
    downloaded_archive_bytes_by_part: RunningByteProgress,
    extracted_bytes_by_archive: RunningByteProgress,
    extract_total_bytes_by_archive: RunningByteProgress,
    log_verified_in_verbose: bool,
}

impl ByteProgressTracker {
    pub fn new(bar: StepProgress, base_total_bytes: u64) -> Self {
        Self {
            bar,
            base_total_bytes,
            downloaded_archive_bytes_by_part: RunningByteProgress::new(),
            extracted_bytes_by_archive: RunningByteProgress::new(),
            extract_total_bytes_by_archive: RunningByteProgress::new(),
            log_verified_in_verbose: false,
        }
    }

    pub fn log_verified_in_verbose(mut self) -> Self {
        self.log_verified_in_verbose = true;
        self
    }

    pub fn handle_event(&mut self, event: &griffr_common::runtime::task_pool::ProgressEvent) {
        match event {
            griffr_common::runtime::task_pool::ProgressEvent::DownloadedBytes { path, .. } => {
                self
                    .downloaded_archive_bytes_by_part
                    .handle_download_event(event)
                    .expect("download event");
                self.update_bar(path);
            }
            griffr_common::runtime::task_pool::ProgressEvent::Downloaded { path, .. } => {
                self
                    .downloaded_archive_bytes_by_part
                    .handle_download_event(event)
                    .expect("download event");
                self.update_bar(path);
            }
            griffr_common::runtime::task_pool::ProgressEvent::ExtractedBytes {
                path,
                bytes,
                total_bytes,
            } => {
                self.extracted_bytes_by_archive.record(path, *bytes);
                self.extract_total_bytes_by_archive.record(path, *total_bytes);
                self.update_bar(path);
            }
            griffr_common::runtime::task_pool::ProgressEvent::Verified { path, ok, .. } => {
                if *ok && self.log_verified_in_verbose && self.bar.verbose {
                    crate::ui::print_info(format!("Verified {}", path));
                }
            }
            _ => {}
        }
    }

    fn update_bar(&self, path: &str) {
        let total_progress_bytes = self
            .downloaded_archive_bytes_by_part
            .total_bytes()
            .saturating_add(self.extracted_bytes_by_archive.total_bytes());
        let total_limit_bytes = self
            .base_total_bytes
            .saturating_add(self.extract_total_bytes_by_archive.total_bytes())
            .max(1);
        self.bar
            .update_bytes(total_progress_bytes, total_limit_bytes, path);
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
        Self {
            bar,
            total_tasks,
            finished_tasks: 0,
        }
    }

    pub fn handle_event(&mut self, event: &griffr_common::runtime::task_pool::ProgressEvent) {
        if let griffr_common::runtime::task_pool::ProgressEvent::Verified { path, .. } = event {
            self.bar.update(self.finished_tasks, self.total_tasks, path);
            self.finished_tasks = self.finished_tasks.saturating_add(1);
        }
    }
}
