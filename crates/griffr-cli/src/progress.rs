//! Progress bar implementation using indicatif.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use griffr_common::runtime::{
    ProgressLane, ProgressReceiver, ProgressSender, ProgressUnit, ProgressUpdate,
};
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Lightweight per-step progress bar.
#[derive(Clone)]
pub struct StepProgress {
    bar: ProgressBar,
    label: String,
    verbose: bool,
    started: Arc<AtomicBool>,
    plain: bool,
    last_plain: Arc<AtomicU64>,
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
            plain: !std::io::stderr().is_terminal(),
            last_plain: Arc::new(AtomicU64::new(u64::MAX)),
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

        if self.plain {
            let completed = completed as u64;
            let total = total as u64;
            let marker = if self.verbose {
                completed
            } else {
                completed
                    .saturating_mul(100)
                    .checked_div(total)
                    .map(|pct| (pct / 5) * 5)
                    .unwrap_or(0)
            };
            if self.last_plain.swap(marker, Ordering::AcqRel) != marker {
                if self.verbose && !file.is_empty() {
                    eprintln!("{}: {}/{} {}", self.label, completed, total, file);
                } else {
                    eprintln!("{}: {}/{}", self.label, completed, total);
                }
            }
            self.bar.set_position(completed);
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
        if self.plain {
            let bucket = ((clamped.saturating_mul(100) / total) / 5) * 5;
            if self.last_plain.swap(bucket, Ordering::AcqRel) != bucket {
                if self.verbose && !file.is_empty() {
                    eprintln!(
                        "{}: {}/{} {}",
                        self.label,
                        HumanBytes(clamped),
                        HumanBytes(total),
                        file
                    );
                } else {
                    eprintln!(
                        "{}: {}/{}",
                        self.label,
                        HumanBytes(clamped),
                        HumanBytes(total)
                    );
                }
            }
            self.bar.set_position(clamped);
            return;
        }
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

    pub fn start(&self, lane: ProgressLane, unit: ProgressUnit) -> ProgressSession {
        ProgressSession::spawn(vec![ProgressRoute {
            lane,
            unit,
            bar: self.clone(),
        }])
    }

    pub fn finish(&self) {
        if !self.started.load(Ordering::Acquire) {
            return;
        }
        let done = self.bar.position();
        let total = self.bar.length().unwrap_or(done);
        if self.plain {
            eprintln!("{}: done ({}/{})", self.label, done, total);
            return;
        }
        self.bar
            .finish_with_message(format!("{} done ({}/{})", self.label, done, total));
    }

    fn ensure_started(&self, total: u64) {
        if !self.started.swap(true, Ordering::AcqRel) {
            if self.plain {
                self.bar.set_length(total);
                return;
            }
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
    plain: bool,
}

impl ActivityProgress {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let plain = !std::io::stderr().is_terminal();
        let bar = ProgressBar::new_spinner();
        if plain {
            bar.set_draw_target(ProgressDrawTarget::hidden());
            eprintln!("{}: started", label);
        } else {
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(20));
            bar.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap(),
            );
            bar.set_message(label.clone());
            bar.enable_steady_tick(Duration::from_millis(120));
        }
        Self { bar, label, plain }
    }

    pub fn finish(&self) {
        if self.plain {
            eprintln!("{}: done", self.label);
        } else {
            self.bar.finish_with_message(format!("{} done", self.label));
        }
    }

    pub fn fail(&self) {
        if self.plain {
            eprintln!("{}: failed", self.label);
        } else {
            self.bar
                .finish_with_message(format!("{} failed", self.label));
        }
    }
}

fn grouped_progress() -> Arc<MultiProgress> {
    Arc::new(MultiProgress::with_draw_target(
        ProgressDrawTarget::stderr_with_hz(20),
    ))
}

/// Owns the renderer thread while common crates emit progress through a channel.
pub struct ProgressSession {
    sender: Option<ProgressSender>,
    worker: Option<JoinHandle<()>>,
}

impl ProgressSession {
    fn spawn(routes: Vec<ProgressRoute>) -> Self {
        let (sender, receiver) = ProgressSender::channel();
        let worker = std::thread::spawn(move || render_updates(receiver, routes));
        Self {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    pub fn sender(&self) -> ProgressSender {
        self.sender
            .as_ref()
            .expect("progress session already finished")
            .clone()
    }

    pub fn finish(mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ProgressSession {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct ProgressRoute {
    lane: ProgressLane,
    unit: ProgressUnit,
    bar: StepProgress,
}

fn render_updates(receiver: ProgressReceiver, routes: Vec<ProgressRoute>) {
    while let Some(update) = receiver.recv() {
        let lane = match &update {
            ProgressUpdate::Started { lane, .. }
            | ProgressUpdate::Advanced { lane, .. }
            | ProgressUpdate::Finished { lane }
            | ProgressUpdate::Failed { lane, .. } => *lane,
        };
        let Some(route) = routes.iter().find(|route| route.lane == lane) else {
            continue;
        };
        apply_update(route, update);
    }
}

fn apply_update(route: &ProgressRoute, update: ProgressUpdate) {
    match update {
        ProgressUpdate::Started { unit, total, .. } if unit == route.unit => {
            if let Some(total) = total {
                match unit {
                    ProgressUnit::Items => route.bar.update_count(0, total as usize, ""),
                    ProgressUnit::Bytes => route.bar.update_bytes(0, total, ""),
                }
            }
        }
        ProgressUpdate::Advanced {
            completed,
            total,
            item,
            ..
        } => {
            let item = item.as_deref().unwrap_or("");
            match route.unit {
                ProgressUnit::Items => {
                    if let Some(total) = total {
                        route
                            .bar
                            .update_count(completed as usize, total as usize, item);
                    }
                }
                ProgressUnit::Bytes => {
                    if let Some(total) = total {
                        route.bar.update_bytes(completed, total, item);
                    }
                }
            }
        }
        ProgressUpdate::Finished { .. } => route.bar.finish(),
        ProgressUpdate::Failed { .. } => route.bar.finish(),
        ProgressUpdate::Started { .. } => {}
    }
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

    pub fn start(&self, count_lane: ProgressLane, byte_lane: ProgressLane) -> ProgressSession {
        ProgressSession::spawn(vec![
            ProgressRoute {
                lane: count_lane,
                unit: ProgressUnit::Items,
                bar: self.count.clone(),
            },
            ProgressRoute {
                lane: byte_lane,
                unit: ProgressUnit::Bytes,
                bar: self.bytes.clone(),
            },
        ])
    }

    pub fn finish(&self) {
        self.count.finish();
        self.bytes.finish();
    }
}

/// Keeps archive verification, network transfer, extraction, and follow-up work on stable rows.
pub struct ArchivePipelineProgress {
    part_count: StepProgress,
    download: StepProgress,
    extract: StepProgress,
    commit: StepProgress,
    patch: StepProgress,
    delete: StepProgress,
}

impl ArchivePipelineProgress {
    pub fn new(label: &str, verbose: bool) -> Self {
        let multi = grouped_progress();
        Self {
            part_count: StepProgress::new_in(
                multi.clone(),
                0,
                format!("{label}.archive-verify"),
                verbose,
            ),
            download: StepProgress::new_in(
                multi.clone(),
                1,
                format!("{label}.archive-download"),
                verbose,
            ),
            extract: StepProgress::new_in(
                multi.clone(),
                2,
                format!("{label}.archive-extract"),
                verbose,
            ),
            commit: StepProgress::new_in(
                multi.clone(),
                3,
                format!("{label}.archive-commit"),
                verbose,
            ),
            patch: StepProgress::new_in(
                multi.clone(),
                4,
                format!("{label}.archive-patch"),
                verbose,
            ),
            delete: StepProgress::new_in(multi, 5, format!("{label}.archive-delete"), verbose),
        }
    }

    pub fn start(
        &self,
        verify_lane: ProgressLane,
        download_lane: ProgressLane,
        extract_lane: ProgressLane,
        commit_lane: ProgressLane,
        patch_lane: ProgressLane,
        delete_lane: ProgressLane,
    ) -> ProgressSession {
        ProgressSession::spawn(vec![
            ProgressRoute {
                lane: verify_lane,
                unit: ProgressUnit::Items,
                bar: self.part_count.clone(),
            },
            ProgressRoute {
                lane: download_lane,
                unit: ProgressUnit::Bytes,
                bar: self.download.clone(),
            },
            ProgressRoute {
                lane: extract_lane,
                unit: ProgressUnit::Bytes,
                bar: self.extract.clone(),
            },
            ProgressRoute {
                lane: commit_lane,
                unit: ProgressUnit::Items,
                bar: self.commit.clone(),
            },
            ProgressRoute {
                lane: patch_lane,
                unit: ProgressUnit::Items,
                bar: self.patch.clone(),
            },
            ProgressRoute {
                lane: delete_lane,
                unit: ProgressUnit::Items,
                bar: self.delete.clone(),
            },
        ])
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
        let count_lane = ProgressLane::INTEGRITY_VERIFY;
        let byte_lane = ProgressLane::INTEGRITY_DOWNLOAD;
        let session = progress.start(count_lane, byte_lane);
        let sender = session.sender();

        sender.emit(ProgressUpdate::Started {
            lane: count_lane,
            unit: ProgressUnit::Items,
            total: Some(10),
        });
        sender.emit(ProgressUpdate::Advanced {
            lane: count_lane,
            completed: 1,
            total: Some(10),
            item: Some("a.bin".to_string()),
        });
        sender.emit(ProgressUpdate::Started {
            lane: byte_lane,
            unit: ProgressUnit::Bytes,
            total: Some(128),
        });
        sender.emit(ProgressUpdate::Advanced {
            lane: byte_lane,
            completed: 64,
            total: Some(128),
            item: Some("b.bin".to_string()),
        });
        drop(sender);
        session.finish();

        assert_eq!(progress.count.bar.position(), 1);
        assert_eq!(progress.count.bar.length(), Some(10));
        assert_eq!(progress.bytes.bar.position(), 64);
        assert_eq!(progress.bytes.bar.length(), Some(128));
    }

    #[test]
    fn archive_pipeline_keeps_download_and_extract_separate() {
        let progress = ArchivePipelineProgress::new("install", false);
        hide_group(&progress.part_count);
        let verify_lane = ProgressLane::ARCHIVE_VERIFY;
        let download_lane = ProgressLane::ARCHIVE_DOWNLOAD;
        let extract_lane = ProgressLane::ARCHIVE_EXTRACT;
        let commit_lane = ProgressLane::ARCHIVE_COMMIT;
        let patch_lane = ProgressLane::ARCHIVE_PATCH;
        let delete_lane = ProgressLane::ARCHIVE_DELETE;
        let session = progress.start(
            verify_lane,
            download_lane,
            extract_lane,
            commit_lane,
            patch_lane,
            delete_lane,
        );
        let sender = session.sender();

        sender.emit(ProgressUpdate::Advanced {
            lane: download_lane,
            completed: 50,
            total: Some(100),
            item: Some("pack.001".to_string()),
        });
        sender.emit(ProgressUpdate::Advanced {
            lane: extract_lane,
            completed: 20,
            total: Some(200),
            item: Some("pack".to_string()),
        });
        drop(sender);
        session.finish();

        assert_eq!(progress.download.bar.position(), 50);
        assert_eq!(progress.download.bar.length(), Some(100));
        assert_eq!(progress.extract.bar.position(), 20);
        assert_eq!(progress.extract.bar.length(), Some(200));
    }
}
