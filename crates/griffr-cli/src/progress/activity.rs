use std::io::IsTerminal;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

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
