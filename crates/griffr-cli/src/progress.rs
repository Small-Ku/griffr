//! Progress bar implementation using indicatif

use indicatif::{ProgressBar, ProgressStyle};

/// Lightweight per-step progress bar (verify/repair/materialize).
#[derive(Clone)]
pub struct StepProgress {
    bar: ProgressBar,
    label: String,
    verbose: bool,
}

impl StepProgress {
    pub fn new(label: impl Into<String>, verbose: bool) -> Self {
        let bar = ProgressBar::new(0);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:40.green/blue}] {pos}/{len}")
                .unwrap()
                .progress_chars("#>-"),
        );
        Self {
            bar,
            label: label.into(),
            verbose,
        }
    }

    pub fn update(&self, current: usize, total: usize, file: &str) {
        let current = current.saturating_add(1);
        let total = total.max(1);
        let total_u64 = total as u64;
        if self.bar.length() != Some(total_u64) {
            self.bar.set_length(total_u64);
        }

        let should_refresh = self.verbose || total <= 10 || current == total || (current % 25 == 0);
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
        self.bar.finish_and_clear();
    }
}
