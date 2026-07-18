use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use compio::dispatcher::Dispatcher;
use futures_util::FutureExt;
use tracing::debug;

use super::executor::{execute_async_task, execute_blocking_task};
use super::types::{
    Task, TaskOutcome, TaskPoolConfig, TaskPoolResult, TaskPoolRunner, TaskProgress, WorkerEvent,
};

const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const COORDINATOR_POLL_INTERVAL: Duration = Duration::from_millis(100);
const BLOCKING_DISPATCH_RETRY_DELAY: Duration = Duration::from_millis(10);
const MAX_IDLE_BLOCKING_DISPATCH_RETRIES: usize = 100;

mod metrics;
mod progress;
mod queue;
mod routing;

use metrics::SchedulerMetrics;
use progress::TaskProgressReducer;
use queue::{ScheduledTask, SchedulerQueue};
use routing::{task_path, task_resources, ExecutionClass, ResourceRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPriority {
    Continuation,
    Bulk,
}

struct TaskCompletion {
    path: String,
    resources: ResourceRequest,
    queue_wait: Duration,
    run_time: Duration,
    spawned: Vec<Task>,
    panicked: bool,
}

enum DispatchAttempt {
    Submitted,
    BlockingPoolBusy(Box<ScheduledTask>),
}

fn record_worker_event(
    progress: &mut TaskProgressReducer,
    outcomes: &mut Vec<TaskOutcome>,
    event: WorkerEvent,
) {
    if let WorkerEvent::Retried { path, reason } = &event {
        debug!(path = %path, reason = %reason, "task retry scheduled");
    }
    progress.handle(&event);
    if let Some(outcome) = event.into_outcome() {
        outcomes.push(outcome);
    }
}

impl TaskPoolRunner {
    pub fn new(config: TaskPoolConfig) -> Result<Self> {
        validate_config(&config)?;
        let mut proactor_builder = compio::driver::ProactorBuilder::new();
        proactor_builder.thread_pool_limit(config.blocking_pool_limit);
        let dispatcher = Arc::new(
            Dispatcher::builder()
                .worker_threads(NonZeroUsize::new(config.dispatcher_threads).ok_or_else(|| {
                    Error::TaskPool("dispatcher threads must be non-zero".to_string())
                })?)
                .proactor_builder(proactor_builder)
                .build()
                .map_err(|error| {
                    Error::TaskPool(format!("Failed to create task-pool dispatcher: {error}"))
                })?,
        );
        let (event_tx, event_rx) = flume::unbounded::<WorkerEvent>();
        Ok(Self {
            config,
            dispatcher,
            event_tx,
            event_rx,
        })
    }

    pub fn run_batch(
        &mut self,
        initial_tasks: Vec<Task>,
        progress: TaskProgress,
    ) -> Result<TaskPoolResult> {
        while self.event_rx.try_recv().is_ok() {}
        let metrics = SchedulerMetrics::default();
        let mut queue = SchedulerQueue::default();
        let mut pending = 0usize;
        for task in initial_tasks {
            enqueue_task(&mut queue, task, TaskPriority::Bulk);
            pending = pending.saturating_add(1);
        }

        let (completion_tx, completion_rx) = flume::unbounded::<TaskCompletion>();
        let mut in_flight = 0usize;
        let mut progress = TaskProgressReducer::new(progress);
        let mut outcomes = Vec::new();
        let mut last_heartbeat_at = Instant::now();
        let mut idle_blocking_dispatch_retries = 0usize;

        while pending > 0 {
            while let Ok(event) = self.event_rx.try_recv() {
                record_worker_event(&mut progress, &mut outcomes, event);
                last_heartbeat_at = Instant::now();
            }

            let mut blocking_pool_busy = false;
            let mut blocking_dispatch_available = true;
            while let Some(scheduled) = queue.pop_next(&self.config, blocking_dispatch_available) {
                match self.dispatch_scheduled(scheduled, completion_tx.clone())? {
                    DispatchAttempt::Submitted => {
                        in_flight = in_flight.saturating_add(1);
                        idle_blocking_dispatch_retries = 0;
                    }
                    DispatchAttempt::BlockingPoolBusy(scheduled) => {
                        queue.restore_front(*scheduled);
                        blocking_pool_busy = true;
                        blocking_dispatch_available = false;
                    }
                }
            }

            if pending == 0 {
                break;
            }

            if in_flight == 0 {
                if blocking_pool_busy {
                    idle_blocking_dispatch_retries =
                        idle_blocking_dispatch_retries.saturating_add(1);
                    if idle_blocking_dispatch_retries > MAX_IDLE_BLOCKING_DISPATCH_RETRIES {
                        return Err(Error::TaskPool(format!(
                            "compio blocking pool remained full with {} queued task(s); \
                             blocking_pool_limit={} cpu_slots={} blocking_slots={}",
                            queue.queued_len(),
                            self.config.blocking_pool_limit,
                            self.config.cpu_slots,
                            self.config.blocking_slots,
                        )));
                    }
                    std::thread::sleep(BLOCKING_DISPATCH_RETRY_DELAY);
                    continue;
                }
                return Err(Error::TaskPool(format!(
                    "task admission deadlock: {} pending task(s), {} queued, none in flight",
                    pending,
                    queue.queued_len(),
                )));
            }

            match completion_rx.recv_timeout(COORDINATOR_POLL_INTERVAL) {
                Ok(completion) => {
                    in_flight = in_flight.saturating_sub(1);
                    pending = pending.saturating_sub(1);
                    queue.release(&completion.resources);
                    metrics.record(
                        completion.queue_wait,
                        completion.run_time,
                        &completion.resources,
                    );
                    if completion.panicked {
                        let _ = self.event_tx.send(WorkerEvent::Failed {
                            path: completion.path,
                            reason: "task execution panicked".to_string(),
                        });
                    } else {
                        for task in completion.spawned {
                            enqueue_task(&mut queue, task, TaskPriority::Continuation);
                            pending = pending.saturating_add(1);
                        }
                    }
                }
                Err(flume::RecvTimeoutError::Timeout)
                    if last_heartbeat_at.elapsed() >= PROGRESS_HEARTBEAT_INTERVAL =>
                {
                    debug!(
                        pending_tasks = pending,
                        in_flight_tasks = in_flight,
                        queued_tasks = queue.queued_len(),
                        "task pool still running without a recent progress event"
                    );
                    last_heartbeat_at = Instant::now();
                }
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => {
                    return Err(Error::TaskPool(
                        "task completion channel disconnected".to_string(),
                    ));
                }
            }
        }

        while let Ok(event) = self.event_rx.try_recv() {
            record_worker_event(&mut progress, &mut outcomes, event);
        }
        progress.finish();
        let metrics = metrics.snapshot();
        debug!(
            completed_tasks = metrics.completed_tasks,
            queue_wait_p50_ms = metrics.queue_wait_p50.as_millis(),
            queue_wait_p95_ms = metrics.queue_wait_p95.as_millis(),
            task_duration_p50_ms = metrics.task_duration_p50.as_millis(),
            task_duration_p95_ms = metrics.task_duration_p95.as_millis(),
            volume_count = metrics.volumes.len(),
            "task pool batch metrics"
        );
        Ok(TaskPoolResult { outcomes, metrics })
    }

    fn dispatch_scheduled(
        &self,
        scheduled: ScheduledTask,
        completion_tx: flume::Sender<TaskCompletion>,
    ) -> Result<DispatchAttempt> {
        let execution = scheduled.resources.execution;
        let job = Arc::new(Mutex::new(Some(scheduled)));
        match execution {
            ExecutionClass::AsyncIo => {
                let job_for_task = Arc::clone(&job);
                let event_tx = self.event_tx.clone();
                let config = self.config.clone();
                match self.dispatcher.dispatch(move || async move {
                    let scheduled = job_for_task
                        .lock()
                        .unwrap()
                        .take()
                        .expect("dispatched async task missing");
                    let ScheduledTask {
                        task,
                        resources,
                        enqueued_at,
                        started_at,
                    } = scheduled;
                    let path = task_path(&task);
                    let queue_wait = started_at.saturating_duration_since(enqueued_at);
                    let mut spawned = Vec::new();
                    let panicked = AssertUnwindSafe(execute_async_task(
                        task,
                        config.max_retries,
                        config.download_progress_buffer_bytes,
                        &config.user_agent,
                        &mut spawned,
                        &event_tx,
                    ))
                    .catch_unwind()
                    .await
                    .is_err();
                    let _ = completion_tx.send(TaskCompletion {
                        path,
                        resources,
                        queue_wait,
                        run_time: started_at.elapsed(),
                        spawned,
                        panicked,
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
                            .expect("rejected async task missing");
                        Err(Error::TaskPool(format!(
                            "Failed to dispatch async I/O task for {}: all dispatcher runtimes stopped",
                            task_path(&scheduled.task)
                        )))
                    }
                }
            }
            ExecutionClass::Cpu | ExecutionClass::Blocking => {
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
                        task,
                        resources,
                        enqueued_at,
                        started_at,
                    } = scheduled;
                    let path = task_path(&task);
                    let queue_wait = started_at.saturating_duration_since(enqueued_at);
                    let mut spawned = Vec::new();
                    let panicked = catch_unwind(AssertUnwindSafe(|| {
                        execute_blocking_task(
                            task,
                            config.max_retries,
                            config.extraction_progress_buffer_bytes,
                            config.extract_shards,
                            &mut spawned,
                            &event_tx,
                        );
                    }))
                    .is_err();
                    let _ = completion_tx.send(TaskCompletion {
                        path,
                        resources,
                        queue_wait,
                        run_time: started_at.elapsed(),
                        spawned,
                        panicked,
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
        ("reuse_pipeline_window", config.reuse_pipeline_window),
    ] {
        if value == 0 {
            return Err(Error::TaskPool(format!("{name} must be non-zero")));
        }
    }
    let admitted_blocking = config.cpu_slots.saturating_add(config.blocking_slots);
    let required_blocking_pool =
        admitted_blocking.saturating_add(super::types::BLOCKING_POOL_INTERNAL_RESERVE);
    if config.blocking_pool_limit < required_blocking_pool {
        return Err(Error::TaskPool(format!(
            "blocking_pool_limit ({}) must cover cpu_slots + blocking_slots ({admitted_blocking}) \
             plus {} reserved compio fallback lanes (minimum {required_blocking_pool})",
            config.blocking_pool_limit,
            super::types::BLOCKING_POOL_INTERNAL_RESERVE,
        )));
    }
    Ok(())
}

pub fn run_tasks_with_progress(
    initial_tasks: Vec<Task>,
    config: TaskPoolConfig,
    progress: TaskProgress,
) -> Result<TaskPoolResult> {
    let mut runner = TaskPoolRunner::new(config)?;
    runner.run_batch(initial_tasks, progress)
}

pub fn run_tasks(initial_tasks: Vec<Task>, config: TaskPoolConfig) -> Result<TaskPoolResult> {
    run_tasks_with_progress(initial_tasks, config, TaskProgress::disabled())
}

fn enqueue_task(queue: &mut SchedulerQueue, task: Task, priority: TaskPriority) {
    let resources = task_resources(&task);
    queue.push(task, resources, priority);
}

#[cfg(test)]
mod progress_tests;

#[cfg(test)]
mod admission_config_tests {
    use super::validate_config;
    use crate::runtime::task_pool::types::BLOCKING_POOL_INTERNAL_RESERVE;
    use crate::runtime::task_pool::TaskPoolConfig;

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
