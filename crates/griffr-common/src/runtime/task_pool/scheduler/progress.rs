use std::collections::HashSet;

use crate::runtime::progress::RunningByteProgress;
use crate::runtime::{ProgressUnit, ProgressUpdate};

use super::super::types::{TaskProgress, WorkerEvent};

pub(super) struct TaskProgressReducer {
    config: TaskProgress,
    verify_finished: u64,
    verified_paths: HashSet<String>,
    download_finished: RunningByteProgress,
    download_totals: RunningByteProgress,
    extract_finished: RunningByteProgress,
    extract_totals: RunningByteProgress,
    download_started: bool,
    extract_started: bool,
    commit_started: bool,
    patch_started: bool,
    delete_started: bool,
}

impl TaskProgressReducer {
    pub(super) fn new(config: TaskProgress) -> Self {
        if let Some((lane, total)) = config.verify {
            if total > 0 {
                config.sender.emit(ProgressUpdate::Started {
                    lane,
                    unit: ProgressUnit::Items,
                    total: Some(total),
                });
            }
        }
        Self {
            config,
            verify_finished: 0,
            verified_paths: HashSet::new(),
            download_finished: RunningByteProgress::new(),
            download_totals: RunningByteProgress::new(),
            extract_finished: RunningByteProgress::new(),
            extract_totals: RunningByteProgress::new(),
            download_started: false,
            extract_started: false,
            commit_started: false,
            patch_started: false,
            delete_started: false,
        }
    }

    pub(super) fn handle(&mut self, event: &WorkerEvent) {
        match event {
            WorkerEvent::Outcome(super::super::types::TaskOutcome::Verified { path, .. }) => {
                if let Some((lane, total)) = self.config.verify {
                    if self.verified_paths.insert(path.clone()) {
                        self.verify_finished = self.verify_finished.saturating_add(1).min(total);
                        self.config.sender.emit(ProgressUpdate::Advanced {
                            lane,
                            finished: self.verify_finished,
                            total: Some(total),
                            item: Some(path.clone()),
                        });
                    }
                }
            }
            WorkerEvent::Outcome(super::super::types::TaskOutcome::Downloaded { path, bytes }) => {
                let Some(lane) = self.config.download else {
                    return;
                };
                self.download_finished.record_max(path, *bytes);
                self.download_totals.record_max(path, *bytes);
                self.start_download_lane(lane, self.download_totals.total_bytes());
                self.emit_bytes(
                    lane,
                    self.download_finished.total_bytes(),
                    self.download_totals.total_bytes(),
                    path,
                );
            }
            WorkerEvent::Progress {
                phase,
                path,
                finished,
                total,
                reset,
            } => match phase {
                crate::runtime::ProgressPhase::Download => {
                    let Some(lane) = self.config.download else {
                        return;
                    };
                    if *reset {
                        self.download_finished.record(path, *finished);
                    } else {
                        self.download_finished.record_max(path, *finished);
                    }
                    if *total > 0 {
                        self.download_totals.record(path, *total);
                    }
                    self.start_download_lane(lane, self.download_totals.total_bytes());
                    self.emit_bytes(
                        lane,
                        self.download_finished.total_bytes(),
                        self.download_totals.total_bytes(),
                        path,
                    );
                }
                crate::runtime::ProgressPhase::Extract => {
                    let Some(lane) = self.config.extract else {
                        return;
                    };
                    self.extract_finished.record_max(path, *finished);
                    if *total > 0 {
                        self.extract_totals.record(path, *total);
                    }
                    if !self.extract_started {
                        self.extract_started = true;
                        self.config.sender.emit(ProgressUpdate::Started {
                            lane,
                            unit: ProgressUnit::Bytes,
                            total: known_total(self.extract_totals.total_bytes()),
                        });
                    }
                    self.emit_bytes(
                        lane,
                        self.extract_finished.total_bytes(),
                        self.extract_totals.total_bytes(),
                        path,
                    );
                }
                crate::runtime::ProgressPhase::Commit => {
                    if let Some(lane) = self.config.commit {
                        Self::emit_items(
                            &self.config.sender,
                            lane,
                            path,
                            *finished,
                            *total,
                            &mut self.commit_started,
                        );
                    }
                }
                crate::runtime::ProgressPhase::Patch => {
                    if let Some(lane) = self.config.patch {
                        Self::emit_items(
                            &self.config.sender,
                            lane,
                            path,
                            *finished,
                            *total,
                            &mut self.patch_started,
                        );
                    }
                }
                crate::runtime::ProgressPhase::Delete => {
                    if let Some(lane) = self.config.delete {
                        Self::emit_items(
                            &self.config.sender,
                            lane,
                            path,
                            *finished,
                            *total,
                            &mut self.delete_started,
                        );
                    }
                }
                crate::runtime::ProgressPhase::Verify => {}
            },
            WorkerEvent::Retried { .. } => {}
            WorkerEvent::Outcome(
                super::super::types::TaskOutcome::ArchiveCheck { .. }
                | super::super::types::TaskOutcome::Changed { .. }
                | super::super::types::TaskOutcome::Hardlinked { .. }
                | super::super::types::TaskOutcome::Copied { .. }
                | super::super::types::TaskOutcome::Failed { .. },
            ) => {}
        }
    }

    fn start_download_lane(&mut self, lane: crate::runtime::ProgressLane, total: u64) {
        if !self.download_started {
            self.download_started = true;
            self.config.sender.emit(ProgressUpdate::Started {
                lane,
                unit: ProgressUnit::Bytes,
                total: known_total(total),
            });
        }
    }

    fn emit_bytes(
        &self,
        lane: crate::runtime::ProgressLane,
        finished: u64,
        total: u64,
        item: &str,
    ) {
        self.config.sender.emit(ProgressUpdate::Advanced {
            lane,
            finished,
            total: known_total(total),
            item: Some(item.to_string()),
        });
    }

    fn emit_items(
        sender: &crate::runtime::ProgressSender,
        lane: crate::runtime::ProgressLane,
        item: &str,
        finished: u64,
        total: u64,
        started: &mut bool,
    ) {
        if !*started {
            *started = true;
            sender.emit(ProgressUpdate::Started {
                lane,
                unit: ProgressUnit::Items,
                total: Some(total),
            });
        }
        sender.emit(ProgressUpdate::Advanced {
            lane,
            finished,
            total: Some(total),
            item: Some(item.to_string()),
        });
    }

    pub(super) fn finish(&self) {
        if let Some((lane, total)) = self.config.verify {
            if total > 0 {
                self.config.sender.emit(ProgressUpdate::Finished { lane });
            }
        }
        for (started, lane) in [
            (self.download_started, self.config.download),
            (self.extract_started, self.config.extract),
            (self.commit_started, self.config.commit),
            (self.patch_started, self.config.patch),
            (self.delete_started, self.config.delete),
        ] {
            if started {
                if let Some(lane) = lane {
                    self.config.sender.emit(ProgressUpdate::Finished { lane });
                }
            }
        }
    }
}

fn known_total(total: u64) -> Option<u64> {
    (total > 0).then_some(total)
}
