use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use crate::error::{Error, Result};

use super::routing::{ExecutionClass, NetworkClass, ResourceRequest};
use super::TaskPriority;
use crate::runtime::task_pool::{Task, TaskPoolConfig};

const CONTINUATION_BURST: usize = 3;
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
}

#[derive(Debug, Default)]
struct ResourceState {
    network_in_use: usize,
    extract_in_use: usize,
    volume_reads: HashMap<String, usize>,
    volume_writes: HashMap<String, usize>,
    mutation_roots: HashSet<String>,
}

impl ResourceState {
    fn can_acquire(&self, request: &ResourceRequest, config: &TaskPoolConfig) -> bool {
        if request.network.is_some() && self.network_in_use >= config.network_slots {
            return false;
        }
        if request.extract && self.extract_in_use >= config.extract_slots {
            return false;
        }
        if request
            .mutation_root
            .as_ref()
            .is_some_and(|root| self.mutation_roots.contains(root))
        {
            return false;
        }
        for volume in &request.read_volumes {
            if self.volume_writes.get(volume).copied().unwrap_or(0) > 0
                || self.volume_reads.get(volume).copied().unwrap_or(0)
                    >= config.volume_read_slots
            {
                return false;
            }
        }
        for volume in &request.write_volumes {
            if self.volume_reads.get(volume).copied().unwrap_or(0) > 0
                || self.volume_writes.get(volume).copied().unwrap_or(0)
                    >= config.volume_write_slots
            {
                return false;
            }
        }
        true
    }

    fn acquire(&mut self, request: &ResourceRequest) {
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
        if let Some(root) = &request.mutation_root {
            self.mutation_roots.insert(root.clone());
        }
    }

    fn release(&mut self, request: &ResourceRequest) {
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
        if let Some(root) = &request.mutation_root {
            self.mutation_roots.remove(root);
        }
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

#[derive(Debug, Default)]
struct QueueState {
    continuation: VecDeque<QueuedTask>,
    bulk: VecDeque<QueuedTask>,
    continuation_streak: [usize; 3],
    network_cursor: usize,
    resources: ResourceState,
}

impl QueueState {
    fn class_index(class: ExecutionClass) -> usize {
        match class {
            ExecutionClass::Network => 0,
            ExecutionClass::Cpu => 1,
            ExecutionClass::Blocking => 2,
        }
    }

    fn pop_runnable(
        &mut self,
        class: ExecutionClass,
        config: &TaskPoolConfig,
    ) -> Option<QueuedTask> {
        let class_index = Self::class_index(class);
        let force_bulk = self.continuation_streak[class_index] >= CONTINUATION_BURST;
        let preferred_network = if class == ExecutionClass::Network {
            let selected = NETWORK_SCHEDULE[self.network_cursor % NETWORK_SCHEDULE.len()];
            self.network_cursor = (self.network_cursor + 1) % NETWORK_SCHEDULE.len();
            Some(selected)
        } else {
            None
        };

        if !force_bulk {
            if let Some(task) = remove_runnable(
                &mut self.continuation,
                class,
                preferred_network,
                &self.resources,
                config,
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
) -> Option<QueuedTask> {
    let preferred = queue.iter().position(|queued| {
        queued.resources.execution == class
            && preferred_network.is_none_or(|network| queued.resources.network == Some(network))
            && resources.can_acquire(&queued.resources, config)
    });
    let fallback = preferred.or_else(|| {
        queue.iter().position(|queued| {
            queued.resources.execution == class
                && resources.can_acquire(&queued.resources, config)
        })
    });
    fallback.and_then(|index| queue.remove(index))
}

#[derive(Debug)]
pub(super) struct ScheduledTask {
    pub(super) task: Task,
    resources: ResourceRequest,
}

#[derive(Debug, Default)]
pub(super) struct SchedulerQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl SchedulerQueue {
    pub(super) fn push(
        &self,
        task: Task,
        resources: ResourceRequest,
        priority: TaskPriority,
        shutdown: &AtomicBool,
    ) -> Result<()> {
        if shutdown.load(Ordering::Acquire) {
            return Err(Error::TaskPool(
                "Failed to enqueue task: task pool is shutting down".to_string(),
            ));
        }
        let mut state = self.state.lock().unwrap();
        let queued = QueuedTask { task, resources };
        match priority {
            TaskPriority::Continuation => state.continuation.push_back(queued),
            TaskPriority::Bulk => state.bulk.push_back(queued),
        }
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    pub(super) fn pop(
        &self,
        class: ExecutionClass,
        config: &TaskPoolConfig,
        shutdown: &AtomicBool,
    ) -> Option<ScheduledTask> {
        let mut state = self.state.lock().unwrap();
        loop {
            if shutdown.load(Ordering::Acquire) {
                return None;
            }
            if let Some(queued) = state.pop_runnable(class, config) {
                state.resources.acquire(&queued.resources);
                return Some(ScheduledTask {
                    task: queued.task,
                    resources: queued.resources,
                });
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    pub(super) fn release(&self, scheduled: &ScheduledTask) {
        let mut state = self.state.lock().unwrap();
        state.resources.release(&scheduled.resources);
        drop(state);
        self.ready.notify_all();
    }

    pub(super) fn notify_all(&self) {
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::SchedulerQueue;
    use crate::runtime::task_pool::scheduler::routing::{
        ExecutionClass, ResourceRequest,
    };
    use crate::runtime::task_pool::scheduler::TaskPriority;
    use crate::runtime::task_pool::{Task, TaskPoolConfig};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

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

    #[test]
    fn volume_writer_blocks_only_the_same_volume() {
        let queue = SchedulerQueue::default();
        let shutdown = AtomicBool::new(false);
        let config = TaskPoolConfig::default();
        queue
            .push(
                hardlink("a"),
                resources("volume-a"),
                TaskPriority::Bulk,
                &shutdown,
            )
            .unwrap();
        queue
            .push(
                hardlink("b"),
                resources("volume-b"),
                TaskPriority::Bulk,
                &shutdown,
            )
            .unwrap();

        let first = queue
            .pop(ExecutionClass::Blocking, &config, &shutdown)
            .unwrap();
        let second = queue
            .pop(ExecutionClass::Blocking, &config, &shutdown)
            .unwrap();
        queue.release(&first);
        queue.release(&second);
    }
}
