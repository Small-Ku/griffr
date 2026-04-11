//! Progress bar implementation using indicatif

use griffr_common::download::ProgressCallback;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Mutex;

/// Indicatif-based progress handler for downloads
pub struct IndicatifProgress {
    multi_progress: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    last_offsets: Mutex<HashMap<String, u64>>,
    total_bar: ProgressBar,
    total_size: u64,
    downloaded: Mutex<u64>,
}

impl IndicatifProgress {
    /// Create a new progress handler with the expected total size
    pub fn new(total_size: u64) -> Self {
        let multi_progress = MultiProgress::new();

        // Create total progress bar
        let total_bar = multi_progress.add(ProgressBar::new(total_size));
        total_bar.set_style(
            ProgressStyle::default_bar()
                .template("{msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );
        total_bar.set_message("Total");

        Self {
            multi_progress,
            bars: Mutex::new(HashMap::new()),
            last_offsets: Mutex::new(HashMap::new()),
            total_bar,
            total_size,
            downloaded: Mutex::new(0),
        }
    }

    /// Get or create a progress bar for a file
    fn get_bar(&self, filename: &str, total_bytes: u64) -> ProgressBar {
        let mut bars = self.bars.lock().unwrap();

        if let Some(bar) = bars.get(filename) {
            bar.clone()
        } else {
            let bar = self.multi_progress.insert(1, ProgressBar::new(total_bytes));
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("{msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            let display_name = if filename.len() > 30 {
                format!("...{}", &filename[filename.len() - 27..])
            } else {
                filename.to_string()
            };
            bar.set_message(display_name);
            bars.insert(filename.to_string(), bar.clone());
            bar
        }
    }
}

impl ProgressCallback for IndicatifProgress {
    fn on_start(&self, filename: &str, total_bytes: u64) {
        let _bar = self.get_bar(filename, total_bytes);
    }

    fn on_progress(&self, filename: &str, downloaded_bytes: u64, _total_bytes: u64) {
        if let Some(bar) = self.bars.lock().unwrap().get(filename) {
            bar.set_position(downloaded_bytes);

            // Calculate delta to avoid over-counting total progress
            let mut offsets = self.last_offsets.lock().unwrap();
            let last = offsets.get(filename).cloned().unwrap_or(0);
            let delta = downloaded_bytes.saturating_sub(last);
            offsets.insert(filename.to_string(), downloaded_bytes);

            if delta > 0 {
                let mut total_downloaded = self.downloaded.lock().unwrap();
                *total_downloaded += delta;
                self.total_bar
                    .set_position((*total_downloaded).min(self.total_size));
            }
        }
    }

    fn on_complete(&self, filename: &str, success: bool) {
        if let Some(bar) = self.bars.lock().unwrap().remove(filename) {
            if success {
                bar.finish_with_message(format!("{} ✓", filename));
            } else {
                bar.finish_with_message(format!("{} ✗", filename));
            }
        }
    }
}
