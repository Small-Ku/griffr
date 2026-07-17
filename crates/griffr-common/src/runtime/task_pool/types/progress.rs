use crate::runtime::{ProgressLane, ProgressSender};

/// Maps task-pool facts onto frontend-neutral progress lanes for one batch.
///
/// A disabled sender keeps every lane unset so non-interactive callers pay no
/// aggregation or allocation cost for transient progress updates.
#[derive(Clone, Default)]
pub struct TaskProgress {
    pub(crate) sender: ProgressSender,
    pub(crate) verify: Option<(ProgressLane, u64)>,
    pub(crate) download: Option<ProgressLane>,
    pub(crate) extract: Option<ProgressLane>,
    pub(crate) commit: Option<ProgressLane>,
    pub(crate) patch: Option<ProgressLane>,
    pub(crate) delete: Option<ProgressLane>,
}

impl TaskProgress {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn new(sender: ProgressSender) -> Self {
        Self {
            sender,
            ..Self::default()
        }
    }

    pub fn with_verify(mut self, lane: ProgressLane, total: usize) -> Self {
        if self.sender.is_enabled() {
            self.verify = Some((lane, total as u64));
        }
        self
    }

    pub fn with_download(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.download = Some(lane);
        }
        self
    }

    pub fn with_extract(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.extract = Some(lane);
        }
        self
    }

    pub fn with_commit(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.commit = Some(lane);
        }
        self
    }

    pub fn with_patch(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.patch = Some(lane);
        }
        self
    }

    pub fn with_delete(mut self, lane: ProgressLane) -> Self {
        if self.sender.is_enabled() {
            self.delete = Some(lane);
        }
        self
    }
}
