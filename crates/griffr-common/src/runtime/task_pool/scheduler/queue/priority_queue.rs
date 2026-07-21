use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use super::super::routing::{ExecutionClass, NetworkClass, ResourceRequest};
use super::super::TaskPriority;
use super::resources::{AdmissionSnapshot, ResourceState};
use crate::runtime::task_pool::{NodeId, Task, TaskPoolConfig};

const CONTINUATION_BURST: usize = 4;
const EXECUTION_SCHEDULE: [ExecutionClass; 4] = [
    ExecutionClass::AsyncIo,
    ExecutionClass::Cpu,
    ExecutionClass::Blocking,
    ExecutionClass::AsyncIo,
];
const NETWORK_SCHEDULE: [NetworkClass; 8] = [
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::Archive,
    NetworkClass::Archive,
    NetworkClass::Vfs,
    NetworkClass::ArchiveBackground,
];

#[derive(Debug)]
pub(super) struct QueuedTask {
    pub(super) node_id: NodeId,
    pub(super) task: Task,
    pub(super) resources: ResourceRequest,
    pub(super) enqueued_at: Instant,
}

#[derive(Debug, Default)]
struct QueueState {
    continuation: VecDeque<QueuedTask>,
    bulk: VecDeque<QueuedTask>,
    continuation_streak: [usize; 3],
    execution_cursor: usize,
    network_cursor: usize,
    resources: ResourceState,
}

impl QueueState {
    fn class_index(class: ExecutionClass) -> usize {
        match class {
            ExecutionClass::AsyncIo => 0,
            ExecutionClass::Cpu => 1,
            ExecutionClass::Blocking => 2,
        }
    }

    fn admission_snapshot(&self, config: &TaskPoolConfig) -> AdmissionSnapshot {
        let mut admission = AdmissionSnapshot::default();
        for queued in self.continuation.iter().chain(&self.bulk) {
            if queued.resources.reuse_probe {
                admission.queued_reuse_commits = admission.queued_reuse_commits.saturating_add(1);
            }
            if (queued.resources.execution == ExecutionClass::Cpu
                && self.resources.cpu_in_use >= config.cpu_slots)
                || (queued.resources.execution == ExecutionClass::Blocking
                    && self.resources.blocking_in_use >= config.blocking_slots)
                || (queued.resources.extract
                    && self.resources.extract_in_use >= config.extract_slots)
                || self
                    .resources
                    .has_mutation_conflict(&queued.resources.mutation_paths)
            {
                continue;
            }
            admission
                .reserved_write_volumes
                .extend(queued.resources.write_volumes.iter().cloned());
        }
        admission
    }

    fn pop_next(
        &mut self,
        config: &TaskPoolConfig,
        blocking_dispatch_available: bool,
    ) -> Option<QueuedTask> {
        for offset in 0..EXECUTION_SCHEDULE.len() {
            let index = (self.execution_cursor + offset) % EXECUTION_SCHEDULE.len();
            let class = EXECUTION_SCHEDULE[index];
            if !blocking_dispatch_available && class != ExecutionClass::AsyncIo {
                continue;
            }
            if let Some(task) = self.pop_runnable(class, config) {
                self.execution_cursor = (index + 1) % EXECUTION_SCHEDULE.len();
                return Some(task);
            }
        }
        None
    }

    fn pop_runnable(
        &mut self,
        class: ExecutionClass,
        config: &TaskPoolConfig,
    ) -> Option<QueuedTask> {
        let class_index = Self::class_index(class);
        let force_bulk = self.continuation_streak[class_index] >= CONTINUATION_BURST;
        let preferred_network = if class == ExecutionClass::AsyncIo {
            let selected = NETWORK_SCHEDULE[self.network_cursor % NETWORK_SCHEDULE.len()];
            self.network_cursor = (self.network_cursor + 1) % NETWORK_SCHEDULE.len();
            Some(selected)
        } else {
            None
        };
        let admission = self.admission_snapshot(config);

        if !force_bulk {
            if let Some(task) = remove_runnable(
                &mut self.continuation,
                class,
                preferred_network,
                &self.resources,
                config,
                &admission,
            ) {
                self.continuation_streak[class_index] =
                    self.continuation_streak[class_index].saturating_add(1);
                return Some(task);
            }
        }
        if let Some(task) = remove_runnable(
            &mut self.bulk,
            class,
            preferred_network,
            &self.resources,
            config,
            &admission,
        ) {
            self.continuation_streak[class_index] = 0;
            return Some(task);
        }
        if force_bulk {
            if let Some(task) = remove_runnable(
                &mut self.continuation,
                class,
                preferred_network,
                &self.resources,
                config,
                &admission,
            ) {
                self.continuation_streak[class_index] = 1;
                return Some(task);
            }
        }
        None
    }
}

fn remove_runnable(
    queue: &mut VecDeque<QueuedTask>,
    class: ExecutionClass,
    preferred_network: Option<NetworkClass>,
    resources: &ResourceState,
    config: &TaskPoolConfig,
    admission: &AdmissionSnapshot,
) -> Option<QueuedTask> {
    let preferred = runnable_index(
        queue,
        class,
        preferred_network,
        resources,
        config,
        admission,
    );
    let fallback =
        preferred.or_else(|| runnable_index(queue, class, None, resources, config, admission));
    fallback.and_then(|index| queue.remove(index))
}

fn runnable_index(
    queue: &VecDeque<QueuedTask>,
    class: ExecutionClass,
    network: Option<NetworkClass>,
    resources: &ResourceState,
    config: &TaskPoolConfig,
    admission: &AdmissionSnapshot,
) -> Option<usize> {
    let mut volume_depth = HashMap::<&str, usize>::new();
    for queued in queue {
        for volume in queued
            .resources
            .read_volumes
            .iter()
            .chain(&queued.resources.write_volumes)
            .chain(&queued.resources.metadata_volumes)
        {
            *volume_depth.entry(volume.as_str()).or_default() += 1;
        }
    }
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| {
            queued.resources.execution == class
                && network.is_none_or(|selected| queued.resources.network == Some(selected))
                && resources.can_acquire(&queued.resources, config, admission)
        })
        .min_by_key(|(index, queued)| {
            let age_bucket = queued.enqueued_at.elapsed().as_secs() / 5;
            let backlog = queued
                .resources
                .read_volumes
                .iter()
                .chain(&queued.resources.write_volumes)
                .chain(&queued.resources.metadata_volumes)
                .map(|volume| volume_depth.get(volume.as_str()).copied().unwrap_or(0))
                .sum::<usize>();
            let reserved_writer_rank = if queued
                .resources
                .write_volumes
                .iter()
                .any(|volume| admission.reserved_write_volumes.contains(volume))
            {
                0
            } else {
                1
            };
            let metadata_rank = if queued.resources.metadata_volumes.is_empty() {
                1
            } else {
                0
            };
            (
                Reverse(age_bucket),
                reserved_writer_rank,
                metadata_rank,
                Reverse(backlog),
                queued.resources.estimated_bytes,
                *index,
            )
        })
        .map(|(index, _)| index)
}

#[derive(Debug)]
pub(crate) struct ScheduledTask {
    pub(crate) node_id: NodeId,
    pub(crate) task: Task,
    pub(crate) resources: ResourceRequest,
    pub(crate) enqueued_at: Instant,
    pub(crate) started_at: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct SchedulerQueue {
    state: QueueState,
}

impl SchedulerQueue {
    pub(crate) fn push(
        &mut self,
        node_id: NodeId,
        task: Task,
        resources: ResourceRequest,
        priority: TaskPriority,
    ) {
        let queued = QueuedTask {
            node_id,
            task,
            resources,
            enqueued_at: Instant::now(),
        };
        match priority {
            TaskPriority::Continuation => self.state.continuation.push_back(queued),
            TaskPriority::Bulk => self.state.bulk.push_back(queued),
        }
    }

    pub(crate) fn restore_front(&mut self, scheduled: ScheduledTask) {
        self.state.resources.release(&scheduled.resources);
        self.state.continuation.push_front(QueuedTask {
            node_id: scheduled.node_id,
            task: scheduled.task,
            resources: scheduled.resources,
            enqueued_at: scheduled.enqueued_at,
        });
    }

    pub(crate) fn pop_next(
        &mut self,
        config: &TaskPoolConfig,
        blocking_dispatch_available: bool,
    ) -> Option<ScheduledTask> {
        let queued = self.state.pop_next(config, blocking_dispatch_available)?;
        self.state.resources.acquire(&queued.resources);
        Some(ScheduledTask {
            node_id: queued.node_id,
            task: queued.task,
            resources: queued.resources,
            enqueued_at: queued.enqueued_at,
            started_at: Instant::now(),
        })
    }

    pub(crate) fn release(&mut self, resources: &ResourceRequest) {
        self.state.resources.release(resources);
    }

    pub(crate) fn queued_len(&self) -> usize {
        self.state
            .continuation
            .len()
            .saturating_add(self.state.bulk.len())
    }
}
