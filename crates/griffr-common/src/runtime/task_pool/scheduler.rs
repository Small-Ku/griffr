use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use compio::dispatcher::Dispatcher;
use flume::{Receiver, RecvTimeoutError, Sender};
use tracing::debug;

use super::executor::execute_task;
use super::types::{
    Task, TaskOutcome, TaskPoolConfig, TaskPoolResult, TaskPoolRunner, TaskProgress, WorkerEvent,
};
use crate::runtime::progress::RunningByteProgress;
use crate::runtime::{ProgressUnit, ProgressUpdate};

const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    Io,
    VfsIo,
    ArchiveIo,
    Cpu,
    Extract,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    pub(crate) io_tx: Sender<Task>,
    pub(crate) vfs_io_tx: Sender<Task>,
    pub(crate) archive_io_tx: Sender<Task>,
    pub(crate) cpu_tx: Sender<Task>,
    pub(crate) extract_tx: Sender<Task>,
    pub(crate) event_tx: Sender<WorkerEvent>,
    pub(crate) pending: Arc<AtomicUsize>,
    pub(crate) done_pair: Arc<(Mutex<()>, Condvar)>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) config: TaskPoolConfig,
    pub(crate) shared_dispatcher: Arc<Dispatcher>,
}

struct TaskProgressReducer {
    config: TaskProgress,
    verify_completed: u64,
    verified_paths: HashSet<String>,
    download_completed: RunningByteProgress,
    download_totals: RunningByteProgress,
    extract_completed: RunningByteProgress,
    extract_totals: RunningByteProgress,
    download_started: bool,
    extract_started: bool,
    commit_started: bool,
    patch_started: bool,
    delete_started: bool,
}

impl TaskProgressReducer {
    fn new(config: TaskProgress) -> Self {
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
            verify_completed: 0,
            verified_paths: HashSet::new(),
            download_completed: RunningByteProgress::new(),
            download_totals: RunningByteProgress::new(),
            extract_completed: RunningByteProgress::new(),
            extract_totals: RunningByteProgress::new(),
            download_started: false,
            extract_started: false,
            commit_started: false,
            patch_started: false,
            delete_started: false,
        }
    }

    fn handle(&mut self, event: &WorkerEvent) {
        match event {
            WorkerEvent::Verified { path, .. } => {
                if let Some((lane, total)) = self.config.verify {
                    if self.verified_paths.insert(path.clone()) {
                        self.verify_completed = self.verify_completed.saturating_add(1).min(total);
                        self.config.sender.emit(ProgressUpdate::Advanced {
                            lane,
                            completed: self.verify_completed,
                            total: Some(total),
                            item: Some(path.clone()),
                        });
                    }
                }
            }
            WorkerEvent::DownloadStarted { path, total_bytes } => {
                let Some(lane) = self.config.download else {
                    return;
                };
                self.download_totals.record(path, *total_bytes);
                self.start_download_lane(lane, self.download_totals.total_bytes());
                self.emit_bytes(
                    lane,
                    self.download_completed.total_bytes(),
                    self.download_totals.total_bytes(),
                    path,
                );
            }
            WorkerEvent::DownloadedBytes {
                path,
                bytes,
                total_bytes,
            } => {
                let Some(lane) = self.config.download else {
                    return;
                };
                self.download_completed.record_max(path, *bytes);
                self.download_totals.record(path, *total_bytes);
                self.start_download_lane(lane, self.download_totals.total_bytes());
                self.emit_bytes(
                    lane,
                    self.download_completed.total_bytes(),
                    self.download_totals.total_bytes(),
                    path,
                );
            }
            WorkerEvent::Downloaded { path, bytes } => {
                let Some(lane) = self.config.download else {
                    return;
                };
                self.download_completed.record_max(path, *bytes);
                self.download_totals.record_max(path, *bytes);
                self.start_download_lane(lane, self.download_totals.total_bytes());
                self.emit_bytes(
                    lane,
                    self.download_completed.total_bytes(),
                    self.download_totals.total_bytes(),
                    path,
                );
            }
            WorkerEvent::ExtractedBytes {
                path,
                bytes,
                total_bytes,
            } => {
                let Some(lane) = self.config.extract else {
                    return;
                };
                self.extract_completed.record_max(path, *bytes);
                self.extract_totals.record(path, *total_bytes);
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
                    self.extract_completed.total_bytes(),
                    self.extract_totals.total_bytes(),
                    path,
                );
            }
            WorkerEvent::ArchiveCommitProgress {
                path,
                completed,
                total,
            } => {
                if let Some(lane) = self.config.commit {
                    Self::emit_items(
                        &self.config.sender,
                        lane,
                        path,
                        *completed,
                        *total,
                        &mut self.commit_started,
                    );
                }
            }
            WorkerEvent::PatchProgress {
                path,
                completed,
                total,
            } => {
                if let Some(lane) = self.config.patch {
                    Self::emit_items(
                        &self.config.sender,
                        lane,
                        path,
                        *completed,
                        *total,
                        &mut self.patch_started,
                    );
                }
            }
            WorkerEvent::DeleteProgress {
                path,
                completed,
                total,
            } => {
                if let Some(lane) = self.config.delete {
                    Self::emit_items(
                        &self.config.sender,
                        lane,
                        path,
                        *completed,
                        *total,
                        &mut self.delete_started,
                    );
                }
            }
            _ => {}
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
        completed: u64,
        total: u64,
        item: &str,
    ) {
        self.config.sender.emit(ProgressUpdate::Advanced {
            lane,
            completed,
            total: known_total(total),
            item: Some(item.to_string()),
        });
    }

    fn emit_items(
        sender: &crate::runtime::ProgressSender,
        lane: crate::runtime::ProgressLane,
        item: &str,
        completed: usize,
        total: usize,
        started: &mut bool,
    ) {
        if !*started {
            *started = true;
            sender.emit(ProgressUpdate::Started {
                lane,
                unit: ProgressUnit::Items,
                total: Some(total as u64),
            });
        }
        sender.emit(ProgressUpdate::Advanced {
            lane,
            completed: completed as u64,
            total: Some(total as u64),
            item: Some(item.to_string()),
        });
    }

    fn finish(&self) {
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

fn known_total(total: u64) -> Option<u64> {
    (total > 0).then_some(total)
}

impl TaskPoolRunner {
    pub fn new(config: TaskPoolConfig) -> Result<Self> {
        let (io_tx, io_rx) = flume::unbounded::<Task>();
        let (vfs_io_tx, vfs_io_rx) = flume::unbounded::<Task>();
        let (archive_io_tx, archive_io_rx) = flume::unbounded::<Task>();
        let (cpu_tx, cpu_rx) = flume::unbounded::<Task>();
        let (extract_tx, extract_rx) = flume::unbounded::<Task>();
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
            io_tx: io_tx.clone(),
            vfs_io_tx: vfs_io_tx.clone(),
            archive_io_tx: archive_io_tx.clone(),
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
        spawn_workers(
            WorkerKind::VfsIo,
            ctx.config.vfs_io_slots,
            vfs_io_rx,
            ctx.clone(),
        )?;
        spawn_workers(
            WorkerKind::ArchiveIo,
            ctx.config.archive_io_slots,
            archive_io_rx,
            ctx.clone(),
        )?;
        spawn_workers(WorkerKind::Cpu, ctx.config.cpu_slots, cpu_rx, ctx.clone())?;
        spawn_workers(
            WorkerKind::Extract,
            ctx.config.extract_slots,
            extract_rx,
            ctx.clone(),
        )?;
        Ok(Self { ctx, event_rx })
    }

    pub fn run_batch(
        &mut self,
        initial_tasks: Vec<Task>,
        progress: TaskProgress,
    ) -> Result<TaskPoolResult> {
        while self.event_rx.try_recv().is_ok() {}
        for task in initial_tasks {
            enqueue_task(&self.ctx, task)?;
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
        let cv = &self.ctx.done_pair.1;
        cv.notify_all();
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
                        worker_ctx.config.patch_slots,
                        worker_ctx.config.extract_shards,
                        worker_ctx.config.commit_slots,
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

mod routing;

use routing::{dispatcher_thread_count, worker_kind_for_task};

pub(crate) fn enqueue_task(ctx: &WorkerContext, task: Task) -> Result<()> {
    ctx.pending.fetch_add(1, Ordering::AcqRel);
    let send_result = match worker_kind_for_task(&task) {
        WorkerKind::Io => ctx.io_tx.send(task),
        WorkerKind::VfsIo => ctx.vfs_io_tx.send(task),
        WorkerKind::ArchiveIo => ctx.archive_io_tx.send(task),
        WorkerKind::Cpu => ctx.cpu_tx.send(task),
        WorkerKind::Extract => ctx.extract_tx.send(task),
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

#[cfg(test)]
mod progress_tests;
