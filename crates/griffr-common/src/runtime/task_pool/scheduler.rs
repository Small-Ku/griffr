use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use compio::dispatcher::Dispatcher;
use flume::{Receiver, RecvTimeoutError, Sender};
use tracing::debug;

use super::executor::execute_task;
use super::types::{ProgressEvent, Task, TaskPoolConfig, TaskPoolResult, TaskPoolRunner};

const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum WorkerKind {
    Io,
    Cpu,
    Extract,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    pub(crate) io_tx: Sender<Task>,
    pub(crate) cpu_tx: Sender<Task>,
    pub(crate) extract_tx: Sender<Task>,
    pub(crate) event_tx: Sender<ProgressEvent>,
    pub(crate) pending: Arc<AtomicUsize>,
    pub(crate) done_pair: Arc<(Mutex<()>, Condvar)>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) config: TaskPoolConfig,
    pub(crate) shared_dispatcher: Arc<Dispatcher>,
}

impl TaskPoolRunner {
    pub fn new(config: TaskPoolConfig) -> Result<Self> {
        let (io_tx, io_rx) = flume::unbounded::<Task>();
        let (cpu_tx, cpu_rx) = flume::unbounded::<Task>();
        let (extract_tx, extract_rx) = flume::unbounded::<Task>();
        let (event_tx, event_rx) = flume::unbounded::<ProgressEvent>();
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
            io_tx: io_tx.clone(),
            cpu_tx: cpu_tx.clone(),
            extract_tx: extract_tx.clone(),
            event_tx: event_tx.clone(),
            pending,
            done_pair,
            shutdown,
            config,
            shared_dispatcher,
        };
        spawn_workers(WorkerKind::Io, ctx.config.io_slots, io_rx, ctx.clone())?;
        spawn_workers(WorkerKind::Cpu, ctx.config.cpu_slots, cpu_rx, ctx.clone())?;
        spawn_workers(
            WorkerKind::Extract,
            ctx.config.extract_slots,
            extract_rx,
            ctx.clone(),
        )?;
        Ok(Self { ctx, event_rx })
    }

    pub fn run_batch_with_progress(
        &mut self,
        initial_tasks: Vec<Task>,
        mut on_event: Option<&mut dyn FnMut(&ProgressEvent)>,
    ) -> Result<TaskPoolResult> {
        while self.event_rx.try_recv().is_ok() {}
        for task in initial_tasks {
            enqueue_task(&self.ctx, task)?;
        }
        let mut events = Vec::new();
        let mut last_heartbeat_at = Instant::now();
        let lock = &self.ctx.done_pair.0;
        let cv = &self.ctx.done_pair.1;
        let mut guard = lock.lock().unwrap();
        loop {
            while let Ok(event) = self.event_rx.try_recv() {
                if let Some(cb) = on_event.as_mut() {
                    cb(&event);
                }
                events.push(event);
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
            if let Some(cb) = on_event.as_mut() {
                cb(&event);
            }
            events.push(event);
        }
        Ok(TaskPoolResult { events })
    }
}

impl Drop for TaskPoolRunner {
    fn drop(&mut self) {
        self.ctx.shutdown.store(true, Ordering::Release);
        let cv = &self.ctx.done_pair.1;
        cv.notify_all();
    }
}

pub fn run_tasks_with_progress(
    initial_tasks: Vec<Task>,
    config: TaskPoolConfig,
    on_event: Option<&mut dyn FnMut(&ProgressEvent)>,
) -> Result<TaskPoolResult> {
    let mut runner = TaskPoolRunner::new(config)?;
    runner.run_batch_with_progress(initial_tasks, on_event)
}

pub fn run_tasks(initial_tasks: Vec<Task>, config: TaskPoolConfig) -> Result<TaskPoolResult> {
    run_tasks_with_progress(initial_tasks, config, None)
}

fn spawn_workers(
    _kind: WorkerKind,
    count: usize,
    rx: Receiver<Task>,
    ctx: WorkerContext,
) -> Result<()> {
    for _ in 0..count {
        let worker_rx = rx.clone();
        let worker_ctx = ctx.clone();
        let dispatcher = Arc::clone(&worker_ctx.shared_dispatcher);
        std::mem::drop(
            dispatcher
                .dispatch_blocking(move || loop {
                    if worker_ctx.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let task = match worker_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(task) => task,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let mut spawned = Vec::new();
                    execute_task(
                        task,
                        worker_ctx.config.max_retries,
                        worker_ctx.config.extraction_progress_buffer_bytes,
                        worker_ctx.config.download_progress_buffer_bytes,
                        Some(worker_ctx.shared_dispatcher.as_ref()),
                        &worker_ctx.config.user_agent,
                        &mut spawned,
                        &worker_ctx.event_tx,
                    );
                    for task in spawned {
                        let _ = enqueue_task(&worker_ctx, task);
                    }
                    let remaining = worker_ctx.pending.fetch_sub(1, Ordering::AcqRel) - 1;
                    if remaining == 0 {
                        let (_, cv) = &*worker_ctx.done_pair;
                        cv.notify_all();
                    }
                })
                .map_err(|_| Error::TaskPool("Failed to dispatch worker loop".to_string()))?,
        );
    }
    Ok(())
}

fn dispatcher_thread_count(config: &TaskPoolConfig) -> usize {
    let worker_loops = config.io_slots + config.cpu_slots + config.extract_slots;
    let extra_io_lanes = config.io_slots.max(1);
    (worker_loops + extra_io_lanes).clamp(2, 64)
}

pub(crate) fn enqueue_task(ctx: &WorkerContext, task: Task) -> Result<()> {
    ctx.pending.fetch_add(1, Ordering::AcqRel);
    let send_result = match task {
        Task::InstallArchive { .. }
        | Task::Download { .. }
        | Task::Hardlink { .. }
        | Task::EnsureFile { .. }
        | Task::ApplyExtractedVfsPatchManifest { .. }
        | Task::ApplyDeleteManifest { .. } => ctx.io_tx.send(task),
        Task::Verify { .. } => ctx.cpu_tx.send(task),
        Task::Extract { .. } => ctx.extract_tx.send(task),
    };
    if send_result.is_err() {
        let remaining = ctx.pending.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining == 0 {
            let (_, cv) = &*ctx.done_pair;
            cv.notify_all();
        }
        return Err(Error::TaskPool(
            "Failed to enqueue task: queue disconnected".to_string(),
        ));
    }
    Ok(())
}
