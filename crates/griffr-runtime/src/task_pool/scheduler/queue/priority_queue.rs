use std::cmp::Reverse;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};

use super::super::routing::{NetworkClass, ResourceRequest, RunClass};
use super::super::TaskPriority;
use super::resources::{AdmissionSnapshot, ResourceState};
use crate::task_pool::{NodeId, Task, TaskPoolConfig};

const CONTINUATION_BURST: usize = 4;
const PRIORITY_LOOKAHEAD: usize = 64;
const RUN_SCHEDULE: [RunClass; 4] = [
    RunClass::AsyncIo,
    RunClass::Cpu,
    RunClass::Blocking,
    RunClass::AsyncIo,
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

#[derive(Debug)]
struct QueueAdmission {
    volume_depth: HashMap<String, usize>,
    run_counts: [usize; 3],
    network_counts: [usize; 4],
}

impl QueueAdmission {
    fn from_queue(queue: &VecDeque<QueuedTask>) -> Self {
        let mut admission = Self {
            volume_depth: HashMap::default(),
            run_counts: [0; 3],
            network_counts: [0; 4],
        };
        for queued in queue {
            admission.add(queued);
        }
        admission
    }

    fn add(&mut self, queued: &QueuedTask) {
        self.run_counts[run_index(queued.resources.run)] =
            self.run_counts[run_index(queued.resources.run)].saturating_add(1);
        if let Some(network) = queued.resources.network {
            self.network_counts[network_index(network)] =
                self.network_counts[network_index(network)].saturating_add(1);
        }
        for volume in queued
            .resources
            .read_volumes
            .iter()
            .chain(&queued.resources.write_volumes)
            .chain(&queued.resources.metadata_volumes)
        {
            *self.volume_depth.entry(volume.clone()).or_default() += 1;
        }
    }

    fn remove(&mut self, queued: &QueuedTask) {
        let run = run_index(queued.resources.run);
        self.run_counts[run] = self.run_counts[run].saturating_sub(1);
        if let Some(network) = queued.resources.network {
            let network = network_index(network);
            self.network_counts[network] = self.network_counts[network].saturating_sub(1);
        }
        for volume in queued
            .resources
            .read_volumes
            .iter()
            .chain(&queued.resources.write_volumes)
            .chain(&queued.resources.metadata_volumes)
        {
            decrement(&mut self.volume_depth, volume);
        }
    }

    fn has_run(&self, class: RunClass) -> bool {
        self.run_counts[run_index(class)] > 0
    }

    fn has_network(&self, network: NetworkClass) -> bool {
        self.network_counts[network_index(network)] > 0
    }
}

#[derive(Debug)]
struct AdmissionCache {
    snapshot: AdmissionSnapshot,
    continuation: QueueAdmission,
    bulk: QueueAdmission,
    reserved_writer_counts: HashMap<String, usize>,
    reserved_writer_nodes: HashSet<NodeId>,
    next_writer_reservation_at: Option<Instant>,
}

impl AdmissionCache {
    fn is_expired(&self, now: Instant) -> bool {
        self.next_writer_reservation_at
            .is_some_and(|deadline| now >= deadline)
    }

    fn remove_task(&mut self, priority: TaskPriority, queued: &QueuedTask) {
        match priority {
            TaskPriority::Continuation => self.continuation.remove(queued),
            TaskPriority::Bulk => self.bulk.remove(queued),
        }

        if queued.resources.reuse_commit {
            self.snapshot.queued_reuse_commits =
                self.snapshot.queued_reuse_commits.saturating_sub(1);
        }

        for reservation in &queued.resources.storage_reservations {
            self.snapshot
                .storage_available_bytes
                .entry(reservation.volume.clone())
                .or_insert_with(|| {
                    crate::available_space(&reservation.probe_path)
                        .ok()
                        .flatten()
                });
        }

        if self.reserved_writer_nodes.remove(&queued.node_id) {
            for volume in &queued.resources.write_volumes {
                decrement(&mut self.reserved_writer_counts, volume);
                if !self.reserved_writer_counts.contains_key(volume) {
                    self.snapshot.reserved_write_volumes.remove(volume);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct QueueState {
    continuation: VecDeque<QueuedTask>,
    bulk: VecDeque<QueuedTask>,
    continuation_streak: [usize; 3],
    run_cursor: usize,
    network_cursor: usize,
    resources: ResourceState,
    admission_cache: Option<AdmissionCache>,
    #[cfg(test)]
    admission_rebuilds: usize,
}

impl QueueState {
    fn invalidate_admission_cache(&mut self) {
        self.admission_cache = None;
    }

    fn ensure_admission_cache(&mut self, config: &TaskPoolConfig) {
        let now = Instant::now();
        if self
            .admission_cache
            .as_ref()
            .is_some_and(|cache| !cache.is_expired(now))
        {
            return;
        }

        let mut snapshot = AdmissionSnapshot::default();
        let mut storage_probe_paths = HashMap::<String, PathBuf>::default();
        let mut reserved_writer_counts = HashMap::<String, usize>::default();
        let mut reserved_writer_nodes = HashSet::default();
        let mut next_writer_reservation_at = None;

        for queued in self.continuation.iter().chain(&self.bulk) {
            for reservation in &queued.resources.storage_reservations {
                storage_probe_paths
                    .entry(reservation.volume.clone())
                    .or_insert_with(|| reservation.probe_path.clone());
            }
            if queued.resources.reuse_commit {
                snapshot.queued_reuse_commits = snapshot.queued_reuse_commits.saturating_add(1);
            }
            if (queued.resources.run == RunClass::Cpu
                && self.resources.cpu_in_use >= config.cpu_slots)
                || (queued.resources.run == RunClass::Blocking
                    && self.resources.blocking_in_use >= config.blocking_slots)
                || (queued.resources.extract
                    && self.resources.extract_in_use >= config.extract_slots)
                || self
                    .resources
                    .has_mutation_conflict(&queued.resources.mutation_paths)
                || queued.resources.write_volumes.is_empty()
            {
                continue;
            }

            let reservation_at = queued
                .enqueued_at
                .checked_add(config.volume_write_reservation_delay)
                .unwrap_or(queued.enqueued_at);
            if now >= reservation_at {
                reserved_writer_nodes.insert(queued.node_id);
                for volume in &queued.resources.write_volumes {
                    *reserved_writer_counts.entry(volume.clone()).or_default() += 1;
                    snapshot.reserved_write_volumes.insert(volume.clone());
                }
            } else {
                next_writer_reservation_at = Some(
                    next_writer_reservation_at.map_or(reservation_at, |current: Instant| {
                        current.min(reservation_at)
                    }),
                );
            }
        }

        for (volume, probe_path) in storage_probe_paths {
            if self
                .resources
                .storage_reserved_bytes
                .get(&volume)
                .copied()
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let available = crate::available_space(&probe_path).ok().flatten();
            snapshot.storage_available_bytes.insert(volume, available);
        }

        self.admission_cache = Some(AdmissionCache {
            snapshot,
            continuation: QueueAdmission::from_queue(&self.continuation),
            bulk: QueueAdmission::from_queue(&self.bulk),
            reserved_writer_counts,
            reserved_writer_nodes,
            next_writer_reservation_at,
        });
        #[cfg(test)]
        {
            self.admission_rebuilds = self.admission_rebuilds.saturating_add(1);
        }
    }

    fn pop_next(
        &mut self,
        config: &TaskPoolConfig,
        blocking_dispatch_available: bool,
    ) -> Option<QueuedTask> {
        self.ensure_admission_cache(config);
        for offset in 0..RUN_SCHEDULE.len() {
            let index = (self.run_cursor + offset) % RUN_SCHEDULE.len();
            let class = RUN_SCHEDULE[index];
            if !blocking_dispatch_available && class != RunClass::AsyncIo {
                continue;
            }
            if let Some(task) = self.pop_runnable(class, config) {
                self.run_cursor = (index + 1) % RUN_SCHEDULE.len();
                return Some(task);
            }
        }
        None
    }

    fn pop_runnable(&mut self, class: RunClass, config: &TaskPoolConfig) -> Option<QueuedTask> {
        let class_index = run_index(class);
        let force_bulk = self.continuation_streak[class_index] >= CONTINUATION_BURST;
        let preferred_network = if class == RunClass::AsyncIo {
            let selected = NETWORK_SCHEDULE[self.network_cursor % NETWORK_SCHEDULE.len()];
            self.network_cursor = (self.network_cursor + 1) % NETWORK_SCHEDULE.len();
            Some(selected)
        } else {
            None
        };
        if !force_bulk {
            if let Some(task) = self.remove_runnable_from(
                TaskPriority::Continuation,
                class,
                preferred_network,
                config,
            ) {
                self.continuation_streak[class_index] =
                    self.continuation_streak[class_index].saturating_add(1);
                return Some(task);
            }
        }
        if let Some(task) =
            self.remove_runnable_from(TaskPriority::Bulk, class, preferred_network, config)
        {
            self.continuation_streak[class_index] = 0;
            return Some(task);
        }
        if force_bulk {
            if let Some(task) = self.remove_runnable_from(
                TaskPriority::Continuation,
                class,
                preferred_network,
                config,
            ) {
                self.continuation_streak[class_index] = 1;
                return Some(task);
            }
        }
        None
    }

    fn remove_runnable_from(
        &mut self,
        priority: TaskPriority,
        class: RunClass,
        preferred_network: Option<NetworkClass>,
        config: &TaskPoolConfig,
    ) -> Option<QueuedTask> {
        let cache = self
            .admission_cache
            .as_ref()
            .expect("admission cache must exist while selecting tasks");
        let (queue, queue_admission) = match priority {
            TaskPriority::Continuation => (&mut self.continuation, &cache.continuation),
            TaskPriority::Bulk => (&mut self.bulk, &cache.bulk),
        };
        if !queue_admission.has_run(class) {
            return None;
        }
        let preferred_network =
            preferred_network.filter(|network| queue_admission.has_network(*network));
        let selected = remove_runnable(
            queue,
            class,
            preferred_network,
            &self.resources,
            config,
            &cache.snapshot,
            &queue_admission.volume_depth,
        );
        if let Some(ref queued) = selected {
            self.admission_cache
                .as_mut()
                .expect("admission cache must exist while selecting tasks")
                .remove_task(priority, queued);
        }
        selected
    }
}

fn remove_runnable(
    queue: &mut VecDeque<QueuedTask>,
    class: RunClass,
    preferred_network: Option<NetworkClass>,
    resources: &ResourceState,
    config: &TaskPoolConfig,
    admission: &AdmissionSnapshot,
    volume_depth: &HashMap<String, usize>,
) -> Option<QueuedTask> {
    let now = Instant::now();
    let preferred = runnable_index(
        queue,
        class,
        preferred_network,
        resources,
        config,
        admission,
        volume_depth,
        now,
    );
    let fallback = preferred.or_else(|| {
        runnable_index(
            queue,
            class,
            None,
            resources,
            config,
            admission,
            volume_depth,
            now,
        )
    });
    fallback.and_then(|index| queue.remove(index))
}

#[allow(clippy::too_many_arguments)]
fn runnable_index(
    queue: &VecDeque<QueuedTask>,
    class: RunClass,
    network: Option<NetworkClass>,
    resources: &ResourceState,
    config: &TaskPoolConfig,
    admission: &AdmissionSnapshot,
    volume_depth: &HashMap<String, usize>,
    now: Instant,
) -> Option<usize> {
    let runnable = |queued: &QueuedTask| {
        queued.resources.run == class
            && network.is_none_or(|selected| queued.resources.network == Some(selected))
            && resources.can_acquire(&queued.resources, config, admission)
    };
    let priority = |index: usize, queued: &QueuedTask| {
        let age_bucket = now.saturating_duration_since(queued.enqueued_at).as_secs() / 5;
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
            index,
        )
    };

    queue
        .iter()
        .enumerate()
        .take(PRIORITY_LOOKAHEAD)
        .filter(|(_, queued)| runnable(queued))
        .min_by_key(|(index, queued)| priority(*index, queued))
        .map(|(index, _)| index)
        .or_else(|| {
            queue
                .iter()
                .enumerate()
                .skip(PRIORITY_LOOKAHEAD)
                .find(|(_, queued)| runnable(queued))
                .map(|(index, _)| index)
        })
}

fn run_index(class: RunClass) -> usize {
    match class {
        RunClass::AsyncIo => 0,
        RunClass::Cpu => 1,
        RunClass::Blocking => 2,
    }
}

fn network_index(class: NetworkClass) -> usize {
    match class {
        NetworkClass::General => 0,
        NetworkClass::Vfs => 1,
        NetworkClass::Archive => 2,
        NetworkClass::ArchiveBackground => 3,
    }
}

fn decrement(counts: &mut HashMap<String, usize>, key: &str) {
    let should_remove = if let Some(count) = counts.get_mut(key) {
        *count = count.saturating_sub(1);
        *count == 0
    } else {
        false
    };
    if should_remove {
        counts.remove(key);
    }
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
        self.state.invalidate_admission_cache();
    }

    pub(crate) fn restore_front(&mut self, scheduled: ScheduledTask) {
        self.state.resources.release(&scheduled.resources);
        self.state.continuation.push_front(QueuedTask {
            node_id: scheduled.node_id,
            task: scheduled.task,
            resources: scheduled.resources,
            enqueued_at: scheduled.enqueued_at,
        });
        self.state.invalidate_admission_cache();
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
        self.state.invalidate_admission_cache();
    }

    pub(crate) fn queued_len(&self) -> usize {
        self.state
            .continuation
            .len()
            .saturating_add(self.state.bulk.len())
    }

    #[cfg(test)]
    pub(crate) fn admission_rebuilds(&self) -> usize {
        self.state.admission_rebuilds
    }
}
