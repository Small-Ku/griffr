use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::routing::{ExecutionClass, NetworkClass, ResourceRequest};
use super::TaskPriority;
use crate::runtime::task_pool::{Task, TaskPoolConfig, VolumeStreamingMode};

const CONTINUATION_BURST: usize = 3;
const EXECUTION_SCHEDULE: [ExecutionClass; 3] = [
    ExecutionClass::AsyncIo,
    ExecutionClass::Cpu,
    ExecutionClass::Blocking,
];
const NETWORK_SCHEDULE: [NetworkClass; 7] = [
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::General,
    NetworkClass::Archive,
    NetworkClass::Archive,
    NetworkClass::Vfs,
];

#[derive(Debug)]
struct QueuedTask {
    task: Task,
    resources: ResourceRequest,
    enqueued_at: Instant,
}

#[derive(Debug, Default)]
struct AdmissionSnapshot {
    reserved_write_volumes: HashSet<String>,
    queued_reuse_commits: usize,
}

#[derive(Debug, Default)]
struct ResourceState {
    network_in_use: usize,
    cpu_in_use: usize,
    blocking_in_use: usize,
    extract_in_use: usize,
    volume_reads: HashMap<String, usize>,
    volume_writes: HashMap<String, usize>,
    volume_metadata: HashMap<String, usize>,
    mutation_roots: HashSet<String>,
    reuse_commits_in_use: usize,
}

impl ResourceState {
    fn can_acquire(
        &self,
        request: &ResourceRequest,
        config: &TaskPoolConfig,
        admission: &AdmissionSnapshot,
    ) -> bool {
        match request.execution {
            ExecutionClass::AsyncIo => {}
            ExecutionClass::Cpu if self.cpu_in_use >= config.cpu_slots => return false,
            ExecutionClass::Blocking if self.blocking_in_use >= config.blocking_slots => {
                return false
            }
            ExecutionClass::Cpu | ExecutionClass::Blocking => {}
        }
        if request.network.is_some() && self.network_in_use >= config.network_slots {
            return false;
        }
        if request.extract && self.extract_in_use >= config.extract_slots {
            return false;
        }
        if request.reuse_probe
            && self
                .reuse_commits_in_use
                .saturating_add(admission.queued_reuse_commits)
                >= config.reuse_pipeline_window.max(1)
        {
            return false;
        }
        if request
            .mutation_root
            .as_ref()
            .is_some_and(|root| self.mutation_roots.contains(root))
        {
            return false;
        }

        for volume in request_volume_set(request) {
            let policy = config.volume_policy(volume);
            let wants_read = request
                .read_volumes
                .iter()
                .any(|item| item.as_str() == volume);
            let wants_write = request
                .write_volumes
                .iter()
                .any(|item| item.as_str() == volume);
            let wants_metadata = request
                .metadata_volumes
                .iter()
                .any(|item| item.as_str() == volume);
            let reads = self.volume_reads.get(volume).copied().unwrap_or(0);
            let writes = self.volume_writes.get(volume).copied().unwrap_or(0);
            let metadata = self.volume_metadata.get(volume).copied().unwrap_or(0);

            if reads.saturating_add(usize::from(wants_read)) > policy.read_limit
                || writes.saturating_add(usize::from(wants_write)) > policy.write_limit
                || metadata.saturating_add(usize::from(wants_metadata)) > policy.metadata_limit
            {
                return false;
            }

            let (current_pressure, requested_pressure) = match policy.streaming_mode {
                VolumeStreamingMode::Exclusive => {
                    if (wants_read && writes > 0)
                        || (wants_write && reads > 0)
                        || (wants_metadata && (reads > 0 || writes > 0))
                        || ((wants_read || wants_write) && metadata > 0)
                    {
                        return false;
                    }
                    if admission.reserved_write_volumes.contains(volume)
                        && !wants_write
                        && wants_metadata
                    {
                        return false;
                    }
                    (reads.max(writes), usize::from(wants_read || wants_write))
                }
                VolumeStreamingMode::Mixed => (
                    reads.saturating_add(writes),
                    usize::from(wants_read).saturating_add(usize::from(wants_write)),
                ),
            };

            let reserve_for_waiting_writer = usize::from(
                admission.reserved_write_volumes.contains(volume)
                    && requested_pressure > 0
                    && !wants_write
                    && writes < policy.write_limit,
            );
            if current_pressure
                .saturating_add(requested_pressure)
                .saturating_add(reserve_for_waiting_writer)
                > policy.streaming_pressure_limit
            {
                return false;
            }
        }
        true
    }

    fn acquire(&mut self, request: &ResourceRequest) {
        match request.execution {
            ExecutionClass::AsyncIo => {}
            ExecutionClass::Cpu => self.cpu_in_use = self.cpu_in_use.saturating_add(1),
            ExecutionClass::Blocking => {
                self.blocking_in_use = self.blocking_in_use.saturating_add(1)
            }
        }
        if request.network.is_some() {
            self.network_in_use = self.network_in_use.saturating_add(1);
        }
        if request.extract {
            self.extract_in_use = self.extract_in_use.saturating_add(1);
        }
        for volume in &request.read_volumes {
            *self.volume_reads.entry(volume.clone()).or_default() += 1;
        }
        for volume in &request.write_volumes {
            *self.volume_writes.entry(volume.clone()).or_default() += 1;
        }
        for volume in &request.metadata_volumes {
            *self.volume_metadata.entry(volume.clone()).or_default() += 1;
        }
        if let Some(root) = &request.mutation_root {
            self.mutation_roots.insert(root.clone());
        }
        if request.reuse_commit {
            self.reuse_commits_in_use = self.reuse_commits_in_use.saturating_add(1);
        }
    }

    fn release(&mut self, request: &ResourceRequest) {
        match request.execution {
            ExecutionClass::AsyncIo => {}
            ExecutionClass::Cpu => self.cpu_in_use = self.cpu_in_use.saturating_sub(1),
            ExecutionClass::Blocking => {
                self.blocking_in_use = self.blocking_in_use.saturating_sub(1)
            }
        }
        if request.network.is_some() {
            self.network_in_use = self.network_in_use.saturating_sub(1);
        }
        if request.extract {
            self.extract_in_use = self.extract_in_use.saturating_sub(1);
        }
        for volume in &request.read_volumes {
            decrement(&mut self.volume_reads, volume);
        }
        for volume in &request.write_volumes {
            decrement(&mut self.volume_writes, volume);
        }
        for volume in &request.metadata_volumes {
            decrement(&mut self.volume_metadata, volume);
        }
        if let Some(root) = &request.mutation_root {
            self.mutation_roots.remove(root);
        }
        if request.reuse_commit {
            self.reuse_commits_in_use = self.reuse_commits_in_use.saturating_sub(1);
        }
    }
}

fn request_volume_set(request: &ResourceRequest) -> BTreeSet<&str> {
    request
        .read_volumes
        .iter()
        .chain(&request.write_volumes)
        .chain(&request.metadata_volumes)
        .map(String::as_str)
        .collect()
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
            if queued.resources.reuse_commit {
                admission.queued_reuse_commits = admission.queued_reuse_commits.saturating_add(1);
            }
            if queued.enqueued_at.elapsed() < config.volume_write_reservation_delay
                || (queued.resources.network.is_some()
                    && self.resources.network_in_use >= config.network_slots)
                || (queued.resources.execution == ExecutionClass::Cpu
                    && self.resources.cpu_in_use >= config.cpu_slots)
                || (queued.resources.execution == ExecutionClass::Blocking
                    && self.resources.blocking_in_use >= config.blocking_slots)
                || (queued.resources.extract
                    && self.resources.extract_in_use >= config.extract_slots)
                || queued
                    .resources
                    .mutation_root
                    .as_ref()
                    .is_some_and(|root| self.resources.mutation_roots.contains(root))
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
pub(super) struct ScheduledTask {
    pub(super) task: Task,
    pub(super) resources: ResourceRequest,
    pub(super) enqueued_at: Instant,
    pub(super) started_at: Instant,
}

#[derive(Debug, Default)]
pub(super) struct SchedulerQueue {
    state: QueueState,
}

impl SchedulerQueue {
    pub(super) fn push(&mut self, task: Task, resources: ResourceRequest, priority: TaskPriority) {
        let queued = QueuedTask {
            task,
            resources,
            enqueued_at: Instant::now(),
        };
        match priority {
            TaskPriority::Continuation => self.state.continuation.push_back(queued),
            TaskPriority::Bulk => self.state.bulk.push_back(queued),
        }
    }

    pub(super) fn restore_front(&mut self, scheduled: ScheduledTask) {
        self.state.resources.release(&scheduled.resources);
        self.state.continuation.push_front(QueuedTask {
            task: scheduled.task,
            resources: scheduled.resources,
            enqueued_at: scheduled.enqueued_at,
        });
    }

    pub(super) fn pop_next(
        &mut self,
        config: &TaskPoolConfig,
        blocking_dispatch_available: bool,
    ) -> Option<ScheduledTask> {
        let queued = self.state.pop_next(config, blocking_dispatch_available)?;
        self.state.resources.acquire(&queued.resources);
        Some(ScheduledTask {
            task: queued.task,
            resources: queued.resources,
            enqueued_at: queued.enqueued_at,
            started_at: Instant::now(),
        })
    }

    pub(super) fn release(&mut self, resources: &ResourceRequest) {
        self.state.resources.release(resources);
    }

    pub(super) fn queued_len(&self) -> usize {
        self.state
            .continuation
            .len()
            .saturating_add(self.state.bulk.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{AdmissionSnapshot, ResourceState, SchedulerQueue};
    use crate::runtime::task_pool::scheduler::routing::{ExecutionClass, ResourceRequest};
    use crate::runtime::task_pool::scheduler::TaskPriority;
    use crate::runtime::task_pool::{Task, TaskPoolConfig, VolumeIoPolicy, VolumeStreamingMode};
    use std::path::PathBuf;

    fn hardlink(name: &str) -> Task {
        Task::Hardlink {
            src: PathBuf::from(format!("{name}.src")),
            dest: PathBuf::from(name),
        }
    }

    fn resources(volume: &str) -> ResourceRequest {
        ResourceRequest {
            execution: ExecutionClass::Blocking,
            write_volumes: vec![volume.to_string()],
            ..ResourceRequest::default()
        }
    }

    fn read(volume: &str) -> ResourceRequest {
        ResourceRequest {
            execution: ExecutionClass::AsyncIo,
            read_volumes: vec![volume.to_string()],
            ..ResourceRequest::default()
        }
    }

    fn write(volume: &str) -> ResourceRequest {
        ResourceRequest {
            execution: ExecutionClass::Blocking,
            write_volumes: vec![volume.to_string()],
            ..ResourceRequest::default()
        }
    }

    fn metadata(volume: &str) -> ResourceRequest {
        ResourceRequest {
            execution: ExecutionClass::Blocking,
            metadata_volumes: vec![volume.to_string()],
            ..ResourceRequest::default()
        }
    }

    #[test]
    fn unavailable_blocking_pool_does_not_stall_async_admission() {
        let mut queue = SchedulerQueue::default();
        let config = TaskPoolConfig::default();
        queue.push(
            hardlink("blocking"),
            ResourceRequest {
                execution: ExecutionClass::Blocking,
                ..ResourceRequest::default()
            },
            TaskPriority::Bulk,
        );
        queue.push(
            hardlink("async"),
            ResourceRequest {
                execution: ExecutionClass::AsyncIo,
                ..ResourceRequest::default()
            },
            TaskPriority::Bulk,
        );

        let selected = queue.pop_next(&config, false).unwrap();
        assert!(matches!(
            selected.task,
            Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("async")
        ));
        queue.release(&selected.resources);
        assert!(queue.pop_next(&config, false).is_none());

        let selected = queue.pop_next(&config, true).unwrap();
        assert!(matches!(
            selected.task,
            Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("blocking")
        ));
        queue.release(&selected.resources);
    }

    #[test]
    fn volume_writer_blocks_only_the_same_volume() {
        let mut queue = SchedulerQueue::default();
        let config = TaskPoolConfig::default();
        queue.push(hardlink("a"), resources("volume-a"), TaskPriority::Bulk);
        queue.push(hardlink("b"), resources("volume-b"), TaskPriority::Bulk);

        let first = queue.pop_next(&config, true).unwrap();
        let second = queue.pop_next(&config, true).unwrap();
        queue.release(&first.resources);
        queue.release(&second.resources);
    }

    #[test]
    fn default_policy_uses_nvme_limits_and_allows_mixed_io() {
        let config = TaskPoolConfig {
            blocking_slots: 200,
            ..TaskPoolConfig::default()
        };
        let admission = AdmissionSnapshot::default();
        let reader = read("volume-a");
        let writer = write("volume-a");
        let metadata = metadata("volume-a");
        let mut state = ResourceState::default();

        assert_eq!(
            config.default_volume_policy,
            VolumeIoPolicy::new(16, 16, 128, 32, VolumeStreamingMode::Mixed)
        );
        for _ in 0..128 {
            state.acquire(&metadata);
        }
        assert!(!state.can_acquire(&metadata, &config, &admission));

        for _ in 0..16 {
            assert!(state.can_acquire(&reader, &config, &admission));
            state.acquire(&reader);
        }
        for _ in 0..16 {
            assert!(state.can_acquire(&writer, &config, &admission));
            state.acquire(&writer);
        }
        assert!(!state.can_acquire(&reader, &config, &admission));
        assert!(!state.can_acquire(&writer, &config, &admission));
    }

    #[test]
    fn same_volume_copy_consumes_both_mixed_streaming_credits() {
        let config = TaskPoolConfig {
            default_volume_policy: VolumeIoPolicy::new(2, 1, 1, 3, VolumeStreamingMode::Mixed),
            ..Default::default()
        };
        let admission = AdmissionSnapshot::default();
        let copy = ResourceRequest {
            read_volumes: vec!["volume-a".to_string()],
            write_volumes: vec!["volume-a".to_string()],
            ..ResourceRequest::default()
        };
        let mut state = ResourceState::default();

        assert!(state.can_acquire(&copy, &config, &admission));
        state.acquire(&copy);
        assert!(state.can_acquire(&read("volume-a"), &config, &admission));
        assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
    }

    #[test]
    fn exclusive_policy_keeps_streaming_and_metadata_separate() {
        let config = TaskPoolConfig {
            default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
            ..Default::default()
        };
        let admission = AdmissionSnapshot::default();
        let reader = read("volume-a");
        let mut state = ResourceState::default();

        state.acquire(&reader);
        assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
        assert!(!state.can_acquire(&metadata("volume-a"), &config, &admission));

        let copy = ResourceRequest {
            read_volumes: vec!["volume-a".to_string()],
            write_volumes: vec!["volume-a".to_string()],
            ..ResourceRequest::default()
        };
        assert!(ResourceState::default().can_acquire(&copy, &config, &admission));
    }

    #[test]
    fn aged_writer_reserves_pressure_without_stopping_all_mixed_mode_readers() {
        let config = TaskPoolConfig::default();
        let admission = AdmissionSnapshot {
            reserved_write_volumes: ["volume-a".to_string()].into_iter().collect(),
            ..AdmissionSnapshot::default()
        };
        let reader = read("volume-a");
        let writer = write("volume-a");
        let mut state = ResourceState::default();

        assert!(state.can_acquire(&reader, &config, &admission));
        state.acquire(&reader);
        assert!(state.can_acquire(&reader, &config, &admission));
        state.acquire(&reader);
        assert!(state.can_acquire(&writer, &config, &admission));
        assert!(state.can_acquire(&metadata("volume-a"), &config, &admission));
    }

    #[test]
    fn aged_writer_stops_new_exclusive_work_until_the_volume_drains() {
        let config = TaskPoolConfig {
            default_volume_policy: VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
            ..Default::default()
        };
        let admission = AdmissionSnapshot {
            reserved_write_volumes: ["volume-a".to_string()].into_iter().collect(),
            ..AdmissionSnapshot::default()
        };

        assert!(!ResourceState::default().can_acquire(&read("volume-a"), &config, &admission,));
        assert!(!ResourceState::default().can_acquire(&metadata("volume-a"), &config, &admission,));
        assert!(ResourceState::default().can_acquire(&write("volume-a"), &config, &admission,));
    }

    #[test]
    fn per_volume_override_can_select_an_exclusive_policy() {
        let mut config = TaskPoolConfig::default();
        config.volume_policies.insert(
            "volume-a".to_string(),
            VolumeIoPolicy::new(1, 1, 1, 1, VolumeStreamingMode::Exclusive),
        );
        let admission = AdmissionSnapshot::default();
        let mut state = ResourceState::default();

        state.acquire(&read("volume-a"));
        assert!(!state.can_acquire(&write("volume-a"), &config, &admission));
        assert!(state.can_acquire(&write("volume-b"), &config, &admission));
    }

    #[test]
    fn reuse_pipeline_window_limits_verified_but_uncommitted_files() {
        let config = TaskPoolConfig {
            reuse_pipeline_window: 1,
            ..Default::default()
        };
        let probe = ResourceRequest {
            reuse_probe: true,
            read_volumes: vec!["volume-a".to_string()],
            ..ResourceRequest::default()
        };
        let admission = AdmissionSnapshot {
            queued_reuse_commits: 1,
            ..AdmissionSnapshot::default()
        };
        assert!(!ResourceState::default().can_acquire(&probe, &config, &admission));
    }

    #[test]
    fn runnable_tasks_prefer_smaller_work_on_the_same_backlogged_volume() {
        let mut queue = SchedulerQueue::default();
        let config = TaskPoolConfig::default();
        let mut large = resources("volume-a");
        large.execution = ExecutionClass::Blocking;
        large.estimated_bytes = 1024;
        let mut small = resources("volume-a");
        small.execution = ExecutionClass::Blocking;
        small.estimated_bytes = 1;
        queue.push(hardlink("large"), large, TaskPriority::Bulk);
        queue.push(hardlink("small"), small, TaskPriority::Bulk);

        let selected = queue.pop_next(&config, true).unwrap();
        assert!(matches!(
            selected.task,
            Task::Hardlink { ref dest, .. } if dest == &PathBuf::from("small")
        ));
        queue.release(&selected.resources);
    }
}
