use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use compio::dispatcher::Dispatcher;
use futures_util::FutureExt;
use tracing::debug;

use super::graph::{ReadyTask, TaskGraph, TaskRun};
use super::runner::{run_async_task, run_blocking_task};
use super::types::{
    Task, TaskOutcome, TaskPoolConfig, TaskPoolResult, TaskPoolRunner, TaskPoolRunnerGroup,
    TaskProgress, WorkerEvent,
};

const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const COORDINATOR_POLL_INTERVAL: Duration = Duration::from_millis(100);
const BLOCKING_DISPATCH_RETRY_DELAY: Duration = Duration::from_millis(10);
const MAX_IDLE_BLOCKING_DISPATCH_RETRIES: usize = 100;
const MAX_WORKER_EVENTS_PER_TICK: usize = 256;
const MAX_TASK_FINISHES_PER_TICK: usize = 256;

mod metrics;
mod progress;
mod queue;
mod routing;

use metrics::SchedulerMetrics;
use progress::TaskProgressReducer;
use queue::{ScheduledTask, SchedulerQueue};
use routing::{
    run_class, task_path, task_resources_cached, ResourceRequest, RunClass, VolumeKeyCache,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPriority {
    Continuation,
    Bulk,
}

struct TaskFinish {
    node_id: super::graph::NodeId,
    path: String,
    resources: ResourceRequest,
    queue_wait: Duration,
    run_time: Duration,
    run: TaskRun,
}

enum DispatchAttempt {
    Submitted,
    BlockingPoolBusy(Box<ScheduledTask>),
}

#[derive(Debug)]
struct ReadyBacklog {
    continuation: [VecDeque<ReadyTask>; 3],
    bulk: [VecDeque<ReadyTask>; 3],
    continuation_cursor: usize,
    bulk_cursor: usize,
    len: usize,
    peak_len: usize,
}

impl Default for ReadyBacklog {
    fn default() -> Self {
        Self {
            continuation: std::array::from_fn(|_| VecDeque::new()),
            bulk: std::array::from_fn(|_| VecDeque::new()),
            continuation_cursor: 0,
            bulk_cursor: 0,
            len: 0,
            peak_len: 0,
        }
    }
}

impl ReadyBacklog {
    fn extend(&mut self, ready: Vec<ReadyTask>) {
        for ready in ready {
            let class = run_class_index(run_class(&ready.task));
            if ready.continuation {
                self.continuation[class].push_back(ready);
            } else {
                self.bulk[class].push_back(ready);
            }
            self.len = self.len.saturating_add(1);
        }
        self.peak_len = self.peak_len.max(self.len);
    }

    fn pop(&mut self) -> Option<ReadyTask> {
        let task = pop_ready_class(&mut self.continuation, &mut self.continuation_cursor)
            .or_else(|| pop_ready_class(&mut self.bulk, &mut self.bulk_cursor));
        if task.is_some() {
            self.len = self.len.saturating_sub(1);
        }
        task
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn run_class_index(class: RunClass) -> usize {
    match class {
        RunClass::AsyncIo => 0,
        RunClass::Cpu => 1,
        RunClass::Blocking => 2,
    }
}

fn pop_ready_class(queues: &mut [VecDeque<ReadyTask>; 3], cursor: &mut usize) -> Option<ReadyTask> {
    for offset in 0..queues.len() {
        let index = cursor.saturating_add(offset) % queues.len();
        if let Some(task) = queues[index].pop_front() {
            *cursor = (index + 1) % queues.len();
            return Some(task);
        }
    }
    None
}

fn record_worker_event(
    progress: &mut TaskProgressReducer,
    outcomes: &mut Vec<TaskOutcome>,
    event: WorkerEvent,
) {
    progress.handle(&event);
    match event {
        WorkerEvent::Retried { path, reason } => {
            debug!(path = %path, reason = %reason, "task retry scheduled");
        }
        WorkerEvent::Outcome(outcome) => outcomes.push(outcome),
        WorkerEvent::Progress { .. } => {}
    }
}

fn complete_task(
    finish: TaskFinish,
    in_flight: &mut usize,
    queue: &mut SchedulerQueue,
    metrics: &SchedulerMetrics,
    graph: &mut TaskGraph,
    event_tx: &flume::Sender<WorkerEvent>,
    ready_backlog: &mut ReadyBacklog,
) -> Result<()> {
    *in_flight = (*in_flight).saturating_sub(1);
    queue.release(&finish.resources);
    metrics.record(finish.queue_wait, finish.run_time, &finish.resources);
    if let Some((reason, report)) = finish.run.failure_details() {
        if report {
            let _ = event_tx.send(WorkerEvent::failed(finish.path.clone(), reason.to_string()));
        }
    }
    let ready = graph.finish(finish.node_id, finish.run)?;
    ready_backlog.extend(ready);
    Ok(())
}

fn complete_task_batch(
    first: TaskFinish,
    finish_rx: &flume::Receiver<TaskFinish>,
    in_flight: &mut usize,
    queue: &mut SchedulerQueue,
    metrics: &SchedulerMetrics,
    graph: &mut TaskGraph,
    event_tx: &flume::Sender<WorkerEvent>,
    ready_backlog: &mut ReadyBacklog,
) -> Result<()> {
    complete_task(
        first,
        in_flight,
        queue,
        metrics,
        graph,
        event_tx,
        ready_backlog,
    )?;
    for _ in 1..MAX_TASK_FINISHES_PER_TICK {
        let Ok(finish) = finish_rx.try_recv() else {
            break;
        };
        complete_task(
            finish,
            in_flight,
            queue,
            metrics,
            graph,
            event_tx,
            ready_backlog,
        )?;
    }
    Ok(())
}

fn build_dispatcher(config: &TaskPoolConfig) -> Result<Arc<Dispatcher>> {
    let mut proactor_builder = compio::driver::ProactorBuilder::new();
    proactor_builder.thread_pool_limit(config.blocking_pool_limit);
    Ok(Arc::new(
        Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(config.dispatcher_threads).ok_or_else(|| {
                Error::Message {
                    context: "Task pool error: ",
                    detail: "dispatcher threads must be non-zero".to_string(),
                }
            })?)
            .proactor_builder(proactor_builder)
            .build()
            .map_err(|error| Error::Message {
                context: "Task pool error: ",
                detail: format!("Failed to create task-pool dispatcher: {error}"),
            })?,
    ))
}

fn runner_with_dispatcher(
    config: TaskPoolConfig,
    dispatcher: Arc<Dispatcher>,
) -> Result<TaskPoolRunner> {
    validate_config(&config)?;
    let (event_tx, event_rx) = flume::unbounded::<WorkerEvent>();
    Ok(TaskPoolRunner {
        config,
        dispatcher,
        event_tx,
        event_rx,
    })
}

impl TaskPoolRunnerGroup {
    pub fn new(config: TaskPoolConfig) -> Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            dispatcher: build_dispatcher(&config)?,
            dispatcher_threads: config.dispatcher_threads,
            blocking_pool_limit: config.blocking_pool_limit,
        })
    }

    pub fn runner(&self, mut config: TaskPoolConfig) -> Result<TaskPoolRunner> {
        config.dispatcher_threads = self.dispatcher_threads;
        config.blocking_pool_limit = self.blocking_pool_limit;
        runner_with_dispatcher(config, self.dispatcher.clone())
    }
}

impl TaskPoolRunner {
    pub fn new(config: TaskPoolConfig) -> Result<Self> {
        validate_config(&config)?;
        let dispatcher = build_dispatcher(&config)?;
        runner_with_dispatcher(config, dispatcher)
    }

    pub fn run_batch(
        &mut self,
        root_tasks: Vec<Task>,
        progress: TaskProgress,
    ) -> Result<TaskPoolResult> {
        self.run_graph(TaskGraph::from_tasks(root_tasks), progress)
    }

    pub fn run_graph(
        &mut self,
        mut graph: TaskGraph,
        progress: TaskProgress,
    ) -> Result<TaskPoolResult> {
        while self.event_rx.try_recv().is_ok() {}
        let run_started_at = Instant::now();
        let metrics = SchedulerMetrics::default();
        let mut queue = SchedulerQueue::default();
        let mut volume_keys = VolumeKeyCache::default();
        let mut ready_backlog = ReadyBacklog::default();
        let graph_start_started_at = Instant::now();
        ready_backlog.extend(graph.start());
        let initial_graph_time = graph_start_started_at.elapsed();
        let ready_frontier_limit = self.config.ready_frontier_limit();
        let initial_routing_started_at = Instant::now();
        route_ready_tasks(
            &mut queue,
            &mut ready_backlog,
            &mut volume_keys,
            ready_frontier_limit,
        );
        let initial_routing_time = initial_routing_started_at.elapsed();
        let (initial_volume_cache_hits, initial_volume_cache_misses) = volume_keys.stats();
        debug!(
            graph_nodes = graph.node_count(),
            ready_frontier_limit,
            initial_graph_ms = initial_graph_time.as_millis(),
            initial_routing_ms = initial_routing_time.as_millis(),
            initial_volume_cache_hits,
            initial_volume_cache_misses,
            ready_backlog = ready_backlog.len,
            "task graph initial routing"
        );

        let (finish_tx, finish_rx) = flume::unbounded::<TaskFinish>();
        let mut in_flight = 0usize;
        let mut first_task_start = None;
        let mut progress = TaskProgressReducer::new(progress);
        let mut outcomes = Vec::new();
        let mut last_heartbeat_at = Instant::now();
        let mut idle_blocking_dispatch_retries = 0usize;

        while graph.has_unresolved() {
            refill_ready_frontier(
                &mut queue,
                &mut ready_backlog,
                &mut volume_keys,
                ready_frontier_limit,
                in_flight,
            );
            let mut worker_events_handled = 0usize;
            for _ in 0..MAX_WORKER_EVENTS_PER_TICK {
                let Ok(event) = self.event_rx.try_recv() else {
                    break;
                };
                record_worker_event(&mut progress, &mut outcomes, event);
                worker_events_handled = worker_events_handled.saturating_add(1);
            }
            if worker_events_handled > 0 {
                last_heartbeat_at = Instant::now();
            }

            let mut blocking_pool_busy = false;
            let mut blocking_dispatch_available = true;
            while let Some(scheduled) = queue.pop_next(&self.config, blocking_dispatch_available) {
                if !graph.is_ready(scheduled.node_id) {
                    queue.release(&scheduled.resources);
                    continue;
                }
                let node_id = scheduled.node_id;
                match self.dispatch_scheduled(scheduled, finish_tx.clone())? {
                    DispatchAttempt::Submitted => {
                        graph.mark_running(node_id)?;
                        in_flight = in_flight.saturating_add(1);
                        first_task_start.get_or_insert_with(|| run_started_at.elapsed());
                        idle_blocking_dispatch_retries = 0;
                    }
                    DispatchAttempt::BlockingPoolBusy(scheduled) => {
                        queue.restore_front(*scheduled);
                        blocking_pool_busy = true;
                        blocking_dispatch_available = false;
                    }
                }
            }

            if !graph.has_unresolved() {
                break;
            }

            if in_flight == 0 {
                if !ready_backlog.is_empty()
                    && route_ready_tasks(
                        &mut queue,
                        &mut ready_backlog,
                        &mut volume_keys,
                        ready_frontier_limit,
                    ) > 0
                {
                    continue;
                }
                if blocking_pool_busy {
                    idle_blocking_dispatch_retries =
                        idle_blocking_dispatch_retries.saturating_add(1);
                    if idle_blocking_dispatch_retries > MAX_IDLE_BLOCKING_DISPATCH_RETRIES {
                        return Err(Error::Message {
                            context: "Task pool error: ",
                            detail: format!(
                                "compio blocking pool remained full with {} queued task(s); \
                             blocking_pool_limit={} cpu_slots={} blocking_slots={}",
                                queue.queued_len(),
                                self.config.blocking_pool_limit,
                                self.config.cpu_slots,
                                self.config.blocking_slots,
                            ),
                        });
                    }
                    std::thread::sleep(BLOCKING_DISPATCH_RETRY_DELAY);
                    continue;
                }
                return Err(Error::Message { context: "Task pool error: ", detail: format!(
                    "task graph admission deadlock: {} unresolved node(s), {} queued, none in flight",
                    graph.unresolved_count(),
                    queue.queued_len(),
                ) });
            }

            if let Ok(finish) = finish_rx.try_recv() {
                complete_task_batch(
                    finish,
                    &finish_rx,
                    &mut in_flight,
                    &mut queue,
                    &metrics,
                    &mut graph,
                    &self.event_tx,
                    &mut ready_backlog,
                )?;
                continue;
            }
            if worker_events_handled == MAX_WORKER_EVENTS_PER_TICK {
                continue;
            }

            match finish_rx.recv_timeout(COORDINATOR_POLL_INTERVAL) {
                Ok(finish) => {
                    complete_task_batch(
                        finish,
                        &finish_rx,
                        &mut in_flight,
                        &mut queue,
                        &metrics,
                        &mut graph,
                        &self.event_tx,
                        &mut ready_backlog,
                    )?;
                }
                Err(flume::RecvTimeoutError::Timeout)
                    if last_heartbeat_at.elapsed() >= PROGRESS_HEARTBEAT_INTERVAL =>
                {
                    debug!(
                        unresolved_nodes = graph.unresolved_count(),
                        in_flight_tasks = in_flight,
                        queued_tasks = queue.queued_len(),
                        "task graph still running without a recent progress event"
                    );
                    last_heartbeat_at = Instant::now();
                }
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => {
                    return Err(Error::Message {
                        context: "Task pool error: ",
                        detail: "task finish channel disconnected".to_string(),
                    });
                }
            }
        }

        while let Ok(event) = self.event_rx.try_recv() {
            record_worker_event(&mut progress, &mut outcomes, event);
        }
        progress.finish();
        let graph_summary = graph.summary();
        let mut metrics = metrics.snapshot();
        metrics.graph = graph_summary.clone();
        metrics.initial_graph_time = initial_graph_time;
        metrics.initial_routing_time = initial_routing_time;
        metrics.first_task_start = first_task_start.unwrap_or_default();
        metrics.ready_frontier_limit = ready_frontier_limit;
        metrics.ready_backlog_peak = ready_backlog.peak_len;
        (metrics.volume_cache_hits, metrics.volume_cache_misses) = volume_keys.stats();
        debug!(
            finished_tasks = metrics.finished_tasks,
            graph_nodes = graph_summary.total_nodes,
            graph_pending = graph_summary.pending_nodes,
            graph_ready = graph_summary.ready_nodes,
            graph_running = graph_summary.running_nodes,
            graph_waiting = graph_summary.waiting_nodes,
            graph_succeeded = graph_summary.succeeded_nodes,
            graph_failed = graph_summary.failed_nodes,
            graph_cancelled = graph_summary.cancelled_nodes,
            graph_expansions = graph_summary.dynamic_expansions,
            queue_wait_p50_ms = metrics.queue_wait_p50.as_millis(),
            queue_wait_p95_ms = metrics.queue_wait_p95.as_millis(),
            task_duration_p50_ms = metrics.task_duration_p50.as_millis(),
            task_duration_p95_ms = metrics.task_duration_p95.as_millis(),
            initial_graph_ms = metrics.initial_graph_time.as_millis(),
            initial_routing_ms = metrics.initial_routing_time.as_millis(),
            first_task_start_ms = metrics.first_task_start.as_millis(),
            ready_frontier_limit = metrics.ready_frontier_limit,
            ready_backlog_peak = metrics.ready_backlog_peak,
            volume_cache_hits = metrics.volume_cache_hits,
            volume_cache_misses = metrics.volume_cache_misses,
            volume_count = metrics.volumes.len(),
            "task graph batch metrics"
        );
        Ok(TaskPoolResult { outcomes, metrics })
    }

    fn dispatch_scheduled(
        &self,
        scheduled: ScheduledTask,
        finish_tx: flume::Sender<TaskFinish>,
    ) -> Result<DispatchAttempt> {
        match scheduled.resources.run {
            RunClass::AsyncIo => {
                let rejected_path = task_path(&scheduled.task);
                let event_tx = self.event_tx.clone();
                let config = self.config.clone();
                match self.dispatcher.dispatch(move || async move {
                    let ScheduledTask {
                        node_id,
                        task,
                        resources,
                        enqueued_at,
                        started_at,
                    } = scheduled;
                    let path = task_path(&task);
                    let queue_wait = started_at.saturating_duration_since(enqueued_at);
                    let run = match AssertUnwindSafe(run_async_task(
                        task,
                        config.max_retries,
                        config.download_progress_buffer_bytes,
                        &config.user_agent,
                        &event_tx,
                    ))
                    .catch_unwind()
                    .await
                    {
                        Ok(run) => run,
                        Err(_) => TaskRun::failed("task run panicked"),
                    };
                    let _ = finish_tx.send(TaskFinish {
                        node_id,
                        path,
                        resources,
                        queue_wait,
                        run_time: started_at.elapsed(),
                        run,
                    });
                }) {
                    Ok(receiver) => {
                        drop(receiver);
                        Ok(DispatchAttempt::Submitted)
                    }
                    Err(error) => {
                        drop(error);
                        Err(Error::Message {
                            context: "Task pool error: ",
                            detail: format!(
                                "Failed to dispatch async I/O task for {rejected_path}: all dispatcher runtimes stopped"
                            ),
                        })
                    }
                }
            }
            RunClass::Cpu | RunClass::Blocking => {
                // dispatch_blocking can reject while its pool is full. Keep the
                // task outside the closure so the coordinator can restore it.
                let job = Arc::new(Mutex::new(Some(scheduled)));
                let job_for_task = Arc::clone(&job);
                let event_tx = self.event_tx.clone();
                let config = self.config.clone();
                match self.dispatcher.dispatch_blocking(move || {
                    let scheduled = job_for_task
                        .lock()
                        .unwrap()
                        .take()
                        .expect("dispatched blocking task missing");
                    let ScheduledTask {
                        node_id,
                        task,
                        resources,
                        enqueued_at,
                        started_at,
                    } = scheduled;
                    let path = task_path(&task);
                    let queue_wait = started_at.saturating_duration_since(enqueued_at);
                    let run = match catch_unwind(AssertUnwindSafe(|| {
                        run_blocking_task(
                            task,
                            config.max_retries,
                            config.extraction_progress_buffer_bytes,
                            config.extract_shards,
                            &event_tx,
                        )
                    })) {
                        Ok(run) => run,
                        Err(_) => TaskRun::failed("task run panicked"),
                    };
                    let _ = finish_tx.send(TaskFinish {
                        node_id,
                        path,
                        resources,
                        queue_wait,
                        run_time: started_at.elapsed(),
                        run,
                    });
                }) {
                    Ok(receiver) => {
                        drop(receiver);
                        Ok(DispatchAttempt::Submitted)
                    }
                    Err(error) => {
                        drop(error);
                        let scheduled = job
                            .lock()
                            .unwrap()
                            .take()
                            .expect("rejected blocking task missing");
                        Ok(DispatchAttempt::BlockingPoolBusy(Box::new(scheduled)))
                    }
                }
            }
        }
    }
}

fn validate_config(config: &TaskPoolConfig) -> Result<()> {
    for (name, value) in [
        ("dispatcher_threads", config.dispatcher_threads),
        ("network_slots", config.network_slots),
        ("cpu_slots", config.cpu_slots),
        ("blocking_slots", config.blocking_slots),
        ("blocking_pool_limit", config.blocking_pool_limit),
        ("extract_slots", config.extract_slots),
        ("reuse_queue_limit", config.reuse_queue_limit),
    ] {
        if value == 0 {
            return Err(Error::Message {
                context: "Task pool error: ",
                detail: format!("{name} must be non-zero"),
            });
        }
    }
    let admitted_blocking = config.cpu_slots.saturating_add(config.blocking_slots);
    let required_blocking_pool =
        admitted_blocking.saturating_add(super::types::BLOCKING_POOL_INTERNAL_RESERVE);
    if config.blocking_pool_limit < required_blocking_pool {
        return Err(Error::Message {
            context: "Task pool error: ",
            detail: format!(
            "blocking_pool_limit ({}) must cover cpu_slots + blocking_slots ({admitted_blocking}) \
             plus {} reserved compio fallback lanes (minimum {required_blocking_pool})",
            config.blocking_pool_limit,
            super::types::BLOCKING_POOL_INTERNAL_RESERVE,
        ),
        });
    }
    Ok(())
}

pub fn run_task_graph_with_progress(
    graph: TaskGraph,
    config: TaskPoolConfig,
    progress: TaskProgress,
) -> Result<TaskPoolResult> {
    let mut runner = TaskPoolRunner::new(config)?;
    runner.run_graph(graph, progress)
}

pub fn run_task_graph(graph: TaskGraph, config: TaskPoolConfig) -> Result<TaskPoolResult> {
    run_task_graph_with_progress(graph, config, TaskProgress::disabled())
}

pub fn run_tasks_with_progress(
    root_tasks: Vec<Task>,
    config: TaskPoolConfig,
    progress: TaskProgress,
) -> Result<TaskPoolResult> {
    run_task_graph_with_progress(TaskGraph::from_tasks(root_tasks), config, progress)
}

pub fn run_tasks(root_tasks: Vec<Task>, config: TaskPoolConfig) -> Result<TaskPoolResult> {
    run_tasks_with_progress(root_tasks, config, TaskProgress::disabled())
}

fn refill_ready_frontier(
    queue: &mut SchedulerQueue,
    ready: &mut ReadyBacklog,
    volume_keys: &mut VolumeKeyCache,
    frontier_limit: usize,
    in_flight: usize,
) -> usize {
    let routed = queue.queued_len().saturating_add(in_flight);
    route_ready_tasks(
        queue,
        ready,
        volume_keys,
        frontier_limit.saturating_sub(routed),
    )
}

fn route_ready_tasks(
    queue: &mut SchedulerQueue,
    ready: &mut ReadyBacklog,
    volume_keys: &mut VolumeKeyCache,
    max_tasks: usize,
) -> usize {
    let mut routed = 0usize;
    while routed < max_tasks {
        let Some(ready) = ready.pop() else {
            break;
        };
        let resources = task_resources_cached(&ready.task, volume_keys);
        let priority = if ready.continuation {
            TaskPriority::Continuation
        } else {
            TaskPriority::Bulk
        };
        queue.push(ready.id, ready.task, resources, priority);
        routed = routed.saturating_add(1);
    }
    routed
}

#[cfg(test)]
mod frontier_tests {
    use super::*;
    use crate::runtime::task_pool::{Task, TaskGraph};
    use std::path::PathBuf;

    fn verify_task(path: PathBuf) -> Task {
        Task::Verify {
            logical_path: path.display().to_string(),
            path,
            expected_md5: "00".repeat(16),
            expected_size: Some(1),
            on_fail: None,
        }
    }

    #[test]
    fn ready_frontier_routes_only_the_requested_window() {
        let temp = tempfile::tempdir().unwrap();
        let mut backlog = ReadyBacklog::default();
        let tasks = (0..300)
            .map(|index| verify_task(temp.path().join(format!("{index}.bin"))))
            .collect();
        let mut graph = TaskGraph::from_tasks(tasks);
        backlog.extend(graph.start());
        let mut queue = SchedulerQueue::default();
        let mut volumes = VolumeKeyCache::default();

        let routed = route_ready_tasks(&mut queue, &mut backlog, &mut volumes, 256);

        assert_eq!(routed, 256);
        assert_eq!(queue.queued_len(), 256);
        assert_eq!(backlog.len, 44);
        assert_eq!(volumes.stats(), (255, 1));
    }

    #[test]
    fn ready_backlog_round_robins_run_classes() {
        let mut backlog = ReadyBacklog::default();
        let mut graph = TaskGraph::from_tasks(vec![
            verify_task(PathBuf::from("cpu.bin")),
            Task::ApplyExtractedVfsPatchManifest {
                install_root: PathBuf::from("blocking"),
            },
            Task::ApplyDeleteManifest {
                install_root: PathBuf::from("async"),
            },
        ]);
        backlog.extend(graph.start());

        assert_eq!(run_class(&backlog.pop().unwrap().task), RunClass::AsyncIo);
        assert_eq!(run_class(&backlog.pop().unwrap().task), RunClass::Cpu);
        assert_eq!(run_class(&backlog.pop().unwrap().task), RunClass::Blocking);
    }
}

#[cfg(test)]
mod progress_tests;

#[cfg(test)]
mod admission_config_tests {
    use super::validate_config;
    use crate::runtime::task_pool::types::BLOCKING_POOL_INTERNAL_RESERVE;
    use crate::runtime::task_pool::TaskPoolConfig;

    #[test]
    fn runner_group_reuses_one_dispatcher() {
        let mut dispatcher_config = TaskPoolConfig::default();
        dispatcher_config.fit_blocking_pool_for_runners(2);
        let group = super::TaskPoolRunnerGroup::new(dispatcher_config).unwrap();
        let runner_config = TaskPoolConfig::default();
        let first = group.runner(runner_config.clone()).unwrap();
        let second = group.runner(runner_config).unwrap();

        assert!(std::sync::Arc::ptr_eq(
            &first.dispatcher,
            &second.dispatcher
        ));
    }

    #[test]
    fn runner_group_rejects_runner_larger_than_its_shared_pool() {
        let mut dispatcher_config = TaskPoolConfig {
            cpu_slots: 1,
            blocking_slots: 2,
            ..TaskPoolConfig::default()
        };
        dispatcher_config.fit_blocking_pool();
        let group = super::TaskPoolRunnerGroup::new(dispatcher_config).unwrap();

        let runner_config = TaskPoolConfig {
            cpu_slots: 12,
            blocking_slots: 8,
            ..TaskPoolConfig::default()
        };
        assert!(group.runner(runner_config).is_err());
    }

    #[test]
    fn blocking_pool_limit_reserves_compio_fallback_capacity() {
        let mut config = TaskPoolConfig::default();
        config.blocking_pool_limit = config
            .cpu_slots
            .saturating_add(config.blocking_slots)
            .saturating_add(BLOCKING_POOL_INTERNAL_RESERVE)
            .saturating_sub(1);

        let error = validate_config(&config).unwrap_err().to_string();
        assert!(error.contains("reserved compio fallback lanes"));
    }
}
