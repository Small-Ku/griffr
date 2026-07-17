use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use compio::dispatcher::Dispatcher;
use flume::Sender;
use tracing::debug;

use super::executor::execute_task;
use super::types::{
    Task, TaskOutcome, TaskPoolConfig, TaskPoolResult, TaskPoolRunner, TaskProgress, WorkerEvent,
};

const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

mod progress;
mod queue;
mod routing;

use progress::TaskProgressReducer;
use queue::WorkerQueue;
use routing::{dispatcher_thread_count, task_path, worker_kind_for_task};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerKind {
    Io,
    VfsIo,
    ArchiveIo,
    Cpu,
    Extract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskPriority {
    Continuation,
    Bulk,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    io_queue: Arc<WorkerQueue>,
    vfs_io_queue: Arc<WorkerQueue>,
    archive_io_queue: Arc<WorkerQueue>,
    cpu_queue: Arc<WorkerQueue>,
    extract_queue: Arc<WorkerQueue>,
    pub(crate) event_tx: Sender<WorkerEvent>,
    pub(crate) pending: Arc<AtomicUsize>,
    pub(crate) done_pair: Arc<(Mutex<()>, Condvar)>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) config: TaskPoolConfig,
    pub(crate) shared_dispatcher: Arc<Dispatcher>,
}

impl WorkerContext {
    fn queue(&self, kind: WorkerKind) -> &WorkerQueue {
        match kind {
            WorkerKind::Io => &self.io_queue,
            WorkerKind::VfsIo => &self.vfs_io_queue,
            WorkerKind::ArchiveIo => &self.archive_io_queue,
            WorkerKind::Cpu => &self.cpu_queue,
            WorkerKind::Extract => &self.extract_queue,
        }
    }

    fn notify_shutdown(&self) {
        self.io_queue.notify_all();
        self.vfs_io_queue.notify_all();
        self.archive_io_queue.notify_all();
        self.cpu_queue.notify_all();
        self.extract_queue.notify_all();
        self.done_pair.1.notify_all();
    }
}

struct PendingTaskGuard {
    ctx: WorkerContext,
}

impl PendingTaskGuard {
    fn new(ctx: &WorkerContext) -> Self {
        Self { ctx: ctx.clone() }
    }
}

impl Drop for PendingTaskGuard {
    fn drop(&mut self) {
        let previous = self.ctx.pending.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "task-pool pending counter underflow");
        if previous <= 1 {
            self.ctx.done_pair.1.notify_all();
        }
    }
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
        let (event_tx, event_rx) = flume::unbounded::<WorkerEvent>();
        let pending = Arc::new(AtomicUsize::new(0));
        let done_pair = Arc::new((Mutex::new(()), Condvar::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let dispatcher_threads = dispatcher_thread_count(&config);
        let shared_dispatcher = Arc::new(
            Dispatcher::builder()
                .worker_threads(NonZeroUsize::new(dispatcher_threads).ok_or_else(|| {
                    Error::TaskPool("dispatcher threads must be non-zero".to_string())
                })?)
                .build()
                .map_err(|e| {
                    Error::TaskPool(format!("Failed to create task-pool dispatcher: {e}"))
                })?,
        );
        let ctx = WorkerContext {
            io_queue: Arc::new(WorkerQueue::default()),
            vfs_io_queue: Arc::new(WorkerQueue::default()),
            archive_io_queue: Arc::new(WorkerQueue::default()),
            cpu_queue: Arc::new(WorkerQueue::default()),
            extract_queue: Arc::new(WorkerQueue::default()),
            event_tx,
            pending,
            done_pair,
            shutdown,
            config,
            shared_dispatcher,
        };

        let mut workers = Vec::new();
        if let Err(error) = spawn_all_workers(&ctx, &mut workers) {
            ctx.shutdown.store(true, Ordering::Release);
            ctx.notify_shutdown();
            for worker in workers {
                let _ = worker.join();
            }
            return Err(error);
        }
        Ok(Self {
            ctx,
            event_rx,
            workers,
        })
    }

    pub fn run_batch(
        &mut self,
        initial_tasks: Vec<Task>,
        progress: TaskProgress,
    ) -> Result<TaskPoolResult> {
        while self.event_rx.try_recv().is_ok() {}
        for task in initial_tasks {
            enqueue_task(&self.ctx, task, TaskPriority::Bulk)?;
        }
        let mut progress = TaskProgressReducer::new(progress);
        let mut outcomes = Vec::new();
        let mut last_heartbeat_at = Instant::now();
        let lock = &self.ctx.done_pair.0;
        let cv = &self.ctx.done_pair.1;
        let mut guard = lock.lock().unwrap();
        loop {
            while let Ok(event) = self.event_rx.try_recv() {
                record_worker_event(&mut progress, &mut outcomes, event);
                last_heartbeat_at = Instant::now();
            }
            let pending = self.ctx.pending.load(Ordering::Acquire);
            if pending == 0 {
                break;
            }
            if last_heartbeat_at.elapsed() >= PROGRESS_HEARTBEAT_INTERVAL {
                debug!(
                    "task pool still running: pending_tasks={} (last progress event >={}s ago)",
                    pending,
                    PROGRESS_HEARTBEAT_INTERVAL.as_secs()
                );
                last_heartbeat_at = Instant::now();
            }
            let (new_guard, _) = cv.wait_timeout(guard, Duration::from_millis(100)).unwrap();
            guard = new_guard;
        }
        drop(guard);
        while let Ok(event) = self.event_rx.try_recv() {
            record_worker_event(&mut progress, &mut outcomes, event);
        }
        progress.finish();
        Ok(TaskPoolResult { outcomes })
    }
}

impl Drop for TaskPoolRunner {
    fn drop(&mut self) {
        self.ctx.shutdown.store(true, Ordering::Release);
        self.ctx.notify_shutdown();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
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

fn spawn_all_workers(ctx: &WorkerContext, workers: &mut Vec<JoinHandle<()>>) -> Result<()> {
    for (kind, count) in [
        (WorkerKind::Io, ctx.config.io_slots),
        (WorkerKind::VfsIo, ctx.config.vfs_io_slots),
        (WorkerKind::ArchiveIo, ctx.config.archive_io_slots),
        (WorkerKind::Cpu, ctx.config.cpu_slots),
        (WorkerKind::Extract, ctx.config.extract_slots),
    ] {
        spawn_workers(kind, count, ctx.clone(), workers)?;
    }
    Ok(())
}

fn spawn_workers(
    kind: WorkerKind,
    count: usize,
    ctx: WorkerContext,
    workers: &mut Vec<JoinHandle<()>>,
) -> Result<()> {
    for index in 0..count {
        let worker_ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name(format!("griffr-task-{kind:?}-{index}"))
            .spawn(move || worker_loop(kind, worker_ctx))
            .map_err(|error| {
                Error::TaskPool(format!("Failed to spawn {kind:?} worker {index}: {error}"))
            })?;
        workers.push(worker);
    }
    Ok(())
}

fn worker_loop(kind: WorkerKind, ctx: WorkerContext) {
    while let Some(task) = ctx.queue(kind).pop(&ctx.shutdown) {
        let _pending = PendingTaskGuard::new(&ctx);
        let failure_path = task_path(&task);
        let mut spawned = Vec::new();
        let result = catch_unwind(AssertUnwindSafe(|| {
            execute_task(
                task,
                ctx.config.max_retries,
                ctx.config.extraction_progress_buffer_bytes,
                ctx.config.download_progress_buffer_bytes,
                ctx.config.patch_slots,
                ctx.config.extract_shards,
                ctx.config.commit_slots,
                Some(ctx.shared_dispatcher.as_ref()),
                &ctx.config.user_agent,
                &mut spawned,
                &ctx.event_tx,
            );
        }));

        if result.is_err() {
            let _ = ctx.event_tx.send(WorkerEvent::Failed {
                path: failure_path,
                reason: "task worker panicked".to_string(),
            });
            continue;
        }

        for task in spawned {
            if let Err(error) = enqueue_task(&ctx, task, TaskPriority::Continuation) {
                let _ = ctx.event_tx.send(WorkerEvent::Failed {
                    path: failure_path.clone(),
                    reason: format!("failed to enqueue continuation: {error}"),
                });
                ctx.shutdown.store(true, Ordering::Release);
                ctx.notify_shutdown();
                break;
            }
        }
    }
}

pub(crate) fn enqueue_task(
    ctx: &WorkerContext,
    task: Task,
    priority: TaskPriority,
) -> Result<()> {
    let kind = worker_kind_for_task(&task);
    ctx.pending.fetch_add(1, Ordering::AcqRel);
    if let Err(error) = ctx.queue(kind).push(task, priority, &ctx.shutdown) {
        let previous = ctx.pending.fetch_sub(1, Ordering::AcqRel);
        if previous <= 1 {
            ctx.done_pair.1.notify_all();
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod progress_tests;
