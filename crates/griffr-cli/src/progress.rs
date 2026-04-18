//! Progress bar implementation using indicatif

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

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
        if !self.started.swap(true, Ordering::AcqRel) {
            self.bar
                .set_draw_target(ProgressDrawTarget::stderr_with_hz(20));
            self.bar.enable_steady_tick(Duration::from_millis(120));
        }
        if self.bar.length() != Some(total_u64) {
            self.bar.set_length(total_u64);
        }

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

    pub fn finish(&self) {
        if !self.started.load(Ordering::Acquire) {
            return;
        }
        let done = self.bar.position();
        let total = self.bar.length().unwrap_or(done);
        self.bar
            .finish_with_message(format!("{} done ({}/{})", self.label, done, total));
    }
}
