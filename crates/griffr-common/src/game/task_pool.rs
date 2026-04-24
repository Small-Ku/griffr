use std::fs::File;
use std::future::Future;
use std::io::ErrorKind;
use std::io::Read;
use std::num::NonZeroUsize;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use compio::buf::BufResult;
use compio::dispatcher::Dispatcher;
use compio::io::AsyncWriteAtExt;
use flume::{Receiver, RecvTimeoutError, Sender};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use tracing::debug;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

use crate::game::{FileIssue, FileIssueKind};

const DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS: u64 = 60;
const DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS: u64 = 15 * 60;
const PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

fn duration_from_env_secs(var: &str, default_secs: u64) -> Duration {
    let secs = std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(default_secs);
    Duration::from_secs(secs)
}

#[derive(Debug, Clone)]
pub enum Task {
    InstallArchive {
        source_dir: PathBuf,
        base_name: String,
        dest: PathBuf,
        cleanup: bool,
        parts: Vec<ArchivePart>,
    },
    Download {
        url: String,
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        retry_count: u32,
    },
    Verify {
        path: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: Option<u64>,
        on_fail: Option<Box<Task>>,
    },
    EnsureFile {
        dest: PathBuf,
        logical_path: String,
        expected_md5: String,
        expected_size: u64,
        source_candidates: Vec<PathBuf>,
        download_url: Option<String>,
        allow_copy_fallback: bool,
        prefer_reuse: bool,
        retry_count: u32,
    },
    Extract {
        source_dir: PathBuf,
        base_name: String,
        dest: PathBuf,
        cleanup: bool,
    },
    Hardlink {
        src: PathBuf,
        dest: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct ArchivePart {
    pub url: String,
    pub dest: PathBuf,
    pub logical_path: String,
    pub expected_md5: String,
    pub expected_size: u64,
}

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    Downloaded {
        path: String,
        bytes: u64,
    },
    Verified {
        path: String,
        ok: bool,
        issue: Option<FileIssue>,
    },
    Retried {
        path: String,
        reason: String,
    },
    Extracted {
        path: PathBuf,
    },
    Hardlinked {
        path: PathBuf,
    },
    Copied {
        path: PathBuf,
    },
    Failed {
        path: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct TaskPoolConfig {
    pub io_slots: usize,
    pub cpu_slots: usize,
    pub extract_slots: usize,
    pub max_retries: u32,
    pub user_agent: String,
}

impl Default for TaskPoolConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            io_slots: (cpus * 2).clamp(2, 16),
            cpu_slots: cpus.clamp(1, 16),
            extract_slots: (cpus / 2).clamp(1, 4),
            max_retries: 3,
            user_agent: "Mozilla/5.0".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct TaskPoolResult {
    pub events: Vec<ProgressEvent>,
}

pub struct TaskPoolRunner {
    ctx: WorkerContext,
    event_rx: Receiver<ProgressEvent>,
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
                .worker_threads(
                    NonZeroUsize::new(dispatcher_threads)
                        .context("dispatcher threads must be non-zero")?,
                )
                .build()
                .context("Failed to create task-pool dispatcher")?,
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
        let (lock, cv) = &*self.ctx.done_pair;
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
        let (_, cv) = &*self.ctx.done_pair;
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

pub fn extract_archives_pooled(
    source_dir: &Path,
    base_names: &[String],
    dest: &Path,
    extract_slots: usize,
    cleanup: bool,
) -> Result<()> {
    if base_names.is_empty() {
        return Ok(());
    }

    let tasks = base_names
        .iter()
        .map(|base| Task::Extract {
            source_dir: source_dir.to_path_buf(),
            base_name: base.clone(),
            dest: dest.to_path_buf(),
            cleanup,
        })
        .collect::<Vec<_>>();

    let mut config = TaskPoolConfig::default();
    config.extract_slots = extract_slots.max(1);
    let result = run_tasks(tasks, config)?;

    let mut failures = Vec::new();
    for event in result.events {
        if let ProgressEvent::Failed { path, reason } = event {
            failures.push(format!("{} ({})", path, reason));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Failed to extract {} archive base(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum WorkerKind {
    Io,
    Cpu,
    Extract,
}

#[derive(Clone)]
struct WorkerContext {
    io_tx: Sender<Task>,
    cpu_tx: Sender<Task>,
    extract_tx: Sender<Task>,
    event_tx: Sender<ProgressEvent>,
    pending: Arc<AtomicUsize>,
    done_pair: Arc<(Mutex<()>, Condvar)>,
    shutdown: Arc<AtomicBool>,
    config: TaskPoolConfig,
    shared_dispatcher: Arc<Dispatcher>,
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
        let _ = dispatcher
            .dispatch_blocking(move || loop {
                if worker_ctx.shutdown.load(Ordering::Acquire) {
                    break;
                }

                let task = match worker_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(task) => task,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                let mut spawned = Vec::new();
                let mut events = Vec::new();
                execute_task(
                    task,
                    &worker_ctx,
                    Some(worker_ctx.shared_dispatcher.as_ref()),
                    &mut spawned,
                    &mut events,
                );

                for event in events {
                    let _ = worker_ctx.event_tx.send(event);
                }
                for task in spawned {
                    let _ = enqueue_task(&worker_ctx, task);
                }

                let remaining = worker_ctx.pending.fetch_sub(1, Ordering::AcqRel) - 1;
                if remaining == 0 {
                    let (_, cv) = &*worker_ctx.done_pair;
                    cv.notify_all();
                }
            })
            .map_err(|_| anyhow::anyhow!("Failed to dispatch worker loop"))?;
    }
    Ok(())
}

fn dispatcher_thread_count(config: &TaskPoolConfig) -> usize {
    // Worker loops are long-lived; reserve additional lanes for nested IO dispatches
    // to avoid starvation when IO workers block waiting for response bodies/filesystem ops.
    let worker_loops = config.io_slots + config.cpu_slots + config.extract_slots;
    let extra_io_lanes = config.io_slots.max(1);
    (worker_loops + extra_io_lanes).clamp(2, 64)
}

fn enqueue_task(ctx: &WorkerContext, task: Task) -> Result<()> {
    ctx.pending.fetch_add(1, Ordering::AcqRel);
    let send_result = match task {
        Task::InstallArchive { .. }
        | Task::Download { .. }
        | Task::Hardlink { .. }
        | Task::EnsureFile { .. } => ctx.io_tx.send(task),
        Task::Verify { .. } => ctx.cpu_tx.send(task),
        Task::Extract { .. } => ctx.extract_tx.send(task),
    };
    if send_result.is_err() {
        let remaining = ctx.pending.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining == 0 {
            let (_, cv) = &*ctx.done_pair;
            cv.notify_all();
        }
        anyhow::bail!("Failed to enqueue task: queue disconnected");
    }
    Ok(())
}

fn execute_task(
    task: Task,
    ctx: &WorkerContext,
    io_dispatcher: Option<&Dispatcher>,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    match task {
        Task::InstallArchive {
            source_dir,
            base_name,
            dest,
            cleanup,
            parts,
        } => execute_install_archive(
            source_dir,
            base_name,
            dest,
            cleanup,
            parts,
            ctx.config.max_retries,
            io_dispatcher,
            &ctx.config.user_agent,
            events,
        ),
        Task::Verify {
            path,
            logical_path,
            expected_md5,
            expected_size,
            on_fail,
        } => execute_verify(
            &path,
            &logical_path,
            &expected_md5,
            expected_size,
            on_fail,
            spawned,
            events,
        ),
        Task::Download {
            url,
            dest,
            logical_path,
            expected_md5,
            expected_size,
            retry_count,
        } => execute_download(
            DownloadExecInput {
                url,
                dest,
                logical_path,
                expected_md5,
                expected_size,
                retry_count,
                max_retries: ctx.config.max_retries,
            },
            io_dispatcher,
            &ctx.config.user_agent,
            spawned,
            events,
        ),
        Task::EnsureFile {
            dest,
            logical_path,
            expected_md5,
            expected_size,
            source_candidates,
            download_url,
            allow_copy_fallback,
            prefer_reuse,
            retry_count,
        } => execute_ensure_file(
            EnsureFileInput {
                dest,
                logical_path,
                expected_md5,
                expected_size,
                source_candidates,
                download_url,
                allow_copy_fallback,
                prefer_reuse,
                retry_count,
                max_retries: ctx.config.max_retries,
            },
            io_dispatcher,
            &ctx.config.user_agent,
            spawned,
            events,
        ),
        Task::Hardlink { src, dest } => match create_hardlink(io_dispatcher, &src, &dest) {
            Ok(()) => events.push(ProgressEvent::Hardlinked { path: dest }),
            Err(err) => events.push(ProgressEvent::Failed {
                path: dest.display().to_string(),
                reason: err.to_string(),
            }),
        },
        Task::Extract {
            source_dir,
            base_name,
            dest,
            cleanup,
        } => execute_extract_archive(source_dir, base_name, dest, cleanup, events),
    }
}

fn execute_install_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    parts: Vec<ArchivePart>,
    max_retries: u32,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    events: &mut Vec<ProgressEvent>,
) {
    for part in parts {
        let mut completed = false;
        for attempt in 0..=max_retries {
            if build_issue(
                &part.dest,
                &part.logical_path,
                &part.expected_md5,
                Some(part.expected_size),
            )
            .is_none()
            {
                events.push(ProgressEvent::Verified {
                    path: part.logical_path.clone(),
                    ok: true,
                    issue: None,
                });
                completed = true;
                break;
            }

            match do_download(
                io_dispatcher,
                user_agent,
                &part.url,
                &part.dest,
                &part.expected_md5,
                Some(part.expected_size),
            ) {
                Ok(bytes) => {
                    events.push(ProgressEvent::Downloaded {
                        path: part.logical_path.clone(),
                        bytes,
                    });
                    let post_issue = build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    if post_issue.is_none() {
                        events.push(ProgressEvent::Verified {
                            path: part.logical_path.clone(),
                            ok: true,
                            issue: None,
                        });
                        completed = true;
                        break;
                    }

                    if attempt < max_retries {
                        events.push(ProgressEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive verify attempt {} failed",
                                attempt + 1
                            ),
                        });
                        continue;
                    }

                    events.push(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue: post_issue,
                    });
                    events.push(ProgressEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: "install-archive verify failed after retries".to_string(),
                    });
                    return;
                }
                Err(err) => {
                    if attempt < max_retries {
                        events.push(ProgressEvent::Retried {
                            path: part.logical_path.clone(),
                            reason: format!(
                                "install-archive download attempt {} failed: {}",
                                attempt + 1,
                                err
                            ),
                        });
                        continue;
                    }

                    let issue = build_issue(
                        &part.dest,
                        &part.logical_path,
                        &part.expected_md5,
                        Some(part.expected_size),
                    );
                    events.push(ProgressEvent::Verified {
                        path: part.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                    events.push(ProgressEvent::Failed {
                        path: part.logical_path.clone(),
                        reason: format!("install-archive download failed after retries: {}", err),
                    });
                    return;
                }
            }
        }

        if !completed {
            return;
        }
    }

    execute_extract_archive(source_dir, base_name, dest, cleanup, events);
}

fn execute_extract_archive(
    source_dir: PathBuf,
    base_name: String,
    dest: PathBuf,
    cleanup: bool,
    events: &mut Vec<ProgressEvent>,
) {
    let result =
        crate::download::extractor::MultiVolumeExtractor::from_directory(&source_dir, &base_name)
            .and_then(|extractor| {
                let staging_dir = make_extract_staging_dir(&dest, &base_name)?;
                std::fs::create_dir_all(&staging_dir).with_context(|| {
                    format!(
                        "Failed to create extraction staging dir {}",
                        staging_dir.display()
                    )
                })?;

                if let Err(err) = extractor.extract_to(&staging_dir) {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(err);
                }

                if let Err(err) = commit_staged_extract(&staging_dir, &dest) {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(err);
                }

                if cleanup {
                    extractor.cleanup()?;
                }
                Ok(())
            });
    match result {
        Ok(()) => events.push(ProgressEvent::Extracted { path: dest }),
        Err(err) => events.push(ProgressEvent::Failed {
            path: format!("{}/{}", source_dir.display(), base_name),
            reason: err.to_string(),
        }),
    }
}

fn execute_verify(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
    on_fail: Option<Box<Task>>,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    let issue = build_issue(path, logical_path, expected_md5, expected_size);
    match issue {
        None => events.push(ProgressEvent::Verified {
            path: logical_path.to_string(),
            ok: true,
            issue: None,
        }),
        Some(issue) => {
            if let Some(task) = on_fail {
                events.push(ProgressEvent::Retried {
                    path: logical_path.to_string(),
                    reason: format!("verification failed ({:?})", issue.kind),
                });
                spawned.push(*task);
                return;
            }

            events.push(ProgressEvent::Verified {
                path: logical_path.to_string(),
                ok: false,
                issue: Some(issue.clone()),
            });
            events.push(ProgressEvent::Failed {
                path: logical_path.to_string(),
                reason: format!("verification failed ({:?})", issue.kind),
            });
        }
    }
}

struct DownloadExecInput {
    url: String,
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: Option<u64>,
    retry_count: u32,
    max_retries: u32,
}

struct EnsureFileInput {
    dest: PathBuf,
    logical_path: String,
    expected_md5: String,
    expected_size: u64,
    source_candidates: Vec<PathBuf>,
    download_url: Option<String>,
    allow_copy_fallback: bool,
    prefer_reuse: bool,
    retry_count: u32,
    max_retries: u32,
}

fn execute_download(
    input: DownloadExecInput,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    let result = do_download(
        io_dispatcher,
        user_agent,
        &input.url,
        &input.dest,
        &input.expected_md5,
        input.expected_size,
    );

    match result {
        Ok(bytes) => {
            events.push(ProgressEvent::Downloaded {
                path: input.logical_path.clone(),
                bytes,
            });
            let on_fail = if input.retry_count < input.max_retries {
                Some(Box::new(Task::Download {
                    url: input.url.clone(),
                    dest: input.dest.clone(),
                    logical_path: input.logical_path.clone(),
                    expected_md5: input.expected_md5.clone(),
                    expected_size: input.expected_size,
                    retry_count: input.retry_count + 1,
                }))
            } else {
                None
            };
            spawned.push(Task::Verify {
                path: input.dest,
                logical_path: input.logical_path,
                expected_md5: input.expected_md5,
                expected_size: input.expected_size,
                on_fail,
            });
        }
        Err(err) => {
            if input.retry_count < input.max_retries {
                events.push(ProgressEvent::Retried {
                    path: input.logical_path.clone(),
                    reason: format!("download attempt {} failed: {}", input.retry_count + 1, err),
                });
                spawned.push(Task::Download {
                    url: input.url,
                    dest: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    retry_count: input.retry_count + 1,
                });
            } else {
                events.push(ProgressEvent::Failed {
                    path: input.logical_path.clone(),
                    reason: format!("download failed after retries: {}", err),
                });
                spawned.push(Task::Verify {
                    path: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    on_fail: None,
                });
            }
        }
    }
}

fn execute_ensure_file(
    input: EnsureFileInput,
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    spawned: &mut Vec<Task>,
    events: &mut Vec<ProgressEvent>,
) {
    let existing_ok = build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    )
    .is_none();
    if existing_ok && !input.prefer_reuse {
        events.push(ProgressEvent::Verified {
            path: input.logical_path,
            ok: true,
            issue: None,
        });
        return;
    }

    let mut reuse_error = None;
    for source in &input.source_candidates {
        if build_issue(
            source,
            &input.logical_path,
            &input.expected_md5,
            Some(input.expected_size),
        )
        .is_some()
        {
            continue;
        }

        match reuse_file(
            io_dispatcher,
            source,
            &input.dest,
            input.allow_copy_fallback,
        ) {
            Ok(ReuseMethod::Hardlink) => {
                events.push(ProgressEvent::Hardlinked {
                    path: input.dest.clone(),
                });
                if build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Ok(ReuseMethod::Copy) => {
                events.push(ProgressEvent::Copied {
                    path: input.dest.clone(),
                });
                if build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                    return;
                }
            }
            Err(err) => reuse_error = Some(err.to_string()),
        }
    }

    if existing_ok {
        events.push(ProgressEvent::Verified {
            path: input.logical_path,
            ok: true,
            issue: None,
        });
        return;
    }

    if let Some(download_url) = &input.download_url {
        match do_download(
            io_dispatcher,
            user_agent,
            download_url,
            &input.dest,
            &input.expected_md5,
            Some(input.expected_size),
        ) {
            Ok(bytes) => {
                events.push(ProgressEvent::Downloaded {
                    path: input.logical_path.clone(),
                    bytes,
                });
                if build_issue(
                    &input.dest,
                    &input.logical_path,
                    &input.expected_md5,
                    Some(input.expected_size),
                )
                .is_none()
                {
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path,
                        ok: true,
                        issue: None,
                    });
                } else {
                    let issue = build_issue(
                        &input.dest,
                        &input.logical_path,
                        &input.expected_md5,
                        Some(input.expected_size),
                    );
                    events.push(ProgressEvent::Verified {
                        path: input.logical_path.clone(),
                        ok: false,
                        issue,
                    });
                }
                return;
            }
            Err(err) if input.retry_count < input.max_retries => {
                events.push(ProgressEvent::Retried {
                    path: input.logical_path.clone(),
                    reason: format!(
                        "ensure-file download attempt {} failed: {}",
                        input.retry_count + 1,
                        err
                    ),
                });
                spawned.push(Task::EnsureFile {
                    dest: input.dest,
                    logical_path: input.logical_path,
                    expected_md5: input.expected_md5,
                    expected_size: input.expected_size,
                    source_candidates: input.source_candidates,
                    download_url: input.download_url,
                    allow_copy_fallback: input.allow_copy_fallback,
                    prefer_reuse: input.prefer_reuse,
                    retry_count: input.retry_count + 1,
                });
                return;
            }
            Err(err) => {
                reuse_error = Some(err.to_string());
            }
        }
    }

    let issue = build_issue(
        &input.dest,
        &input.logical_path,
        &input.expected_md5,
        Some(input.expected_size),
    );
    events.push(ProgressEvent::Verified {
        path: input.logical_path.clone(),
        ok: false,
        issue,
    });
    events.push(ProgressEvent::Failed {
        path: input.logical_path,
        reason: reuse_error.unwrap_or_else(|| "ensure-file failed".to_string()),
    });
}

fn do_download(
    io_dispatcher: Option<&Dispatcher>,
    user_agent: &str,
    url: &str,
    dest: &Path,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Result<u64> {
    let send_timeout = duration_from_env_secs(
        "GRIFFR_DOWNLOAD_SEND_TIMEOUT_SECS",
        DEFAULT_DOWNLOAD_SEND_TIMEOUT_SECS,
    );
    let body_timeout = duration_from_env_secs(
        "GRIFFR_DOWNLOAD_BODY_TIMEOUT_SECS",
        DEFAULT_DOWNLOAD_BODY_TIMEOUT_SECS,
    );
    let part_path = make_partial_download_path(dest)?;
    let part_path_for_resume = part_path.clone();
    let resume_from = dispatch_io(io_dispatcher, move || async move {
        match compio::fs::metadata(&part_path_for_resume).await {
            Ok(metadata) => Ok::<Option<u64>, anyhow::Error>(match expected_size {
                Some(size) if metadata.len() < size => Some(metadata.len()),
                Some(_) => Some(0),
                None => Some(metadata.len()),
            }),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "Failed to stat partial download file {}",
                    part_path_for_resume.display()
                )
            }),
        }
    })?;

    let url_owned = url.to_string();
    let user_agent_owned = user_agent.to_string();
    let part_path_for_write = part_path.clone();
    let (written, actual_md5) = dispatch_io(io_dispatcher, move || async move {
        let client = cyper::Client::new();
        let mut request = client
            .get(&url_owned)
            .with_context(|| format!("Failed to build request for {}", url_owned))?;
        request = request
            .header("User-Agent", user_agent_owned)
            .context("Failed to attach User-Agent header")?;
        if let Some(offset) = resume_from.filter(|o| *o > 0) {
            request = request
                .header("Range", format!("bytes={}-", offset))
                .context("Failed to set Range header for resume")?;
            debug!("resuming download from byte {} for {}", offset, url_owned);
        }
        let response = compio::time::timeout(send_timeout, request.send())
            .await
            .with_context(|| format!("Timed out waiting for response from {}", url_owned))?
            .with_context(|| format!("Failed to download {}", url_owned))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("HTTP error {}", status);
        }

        if let Some(parent) = part_path_for_write.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }

        let resume_effective = resume_from.filter(|o| *o > 0).is_some() && status.as_u16() == 206;
        let resume_offset = resume_from.unwrap_or(0);
        let mut open_options = compio::fs::OpenOptions::new();
        open_options
            .create(true)
            .write(true)
            .truncate(!resume_effective);
        let mut out = open_options
            .open(&part_path_for_write)
            .await
            .with_context(|| {
                format!(
                    "Failed to open partial download file {}",
                    part_path_for_write.display()
                )
            })?;

        let mut hasher = Md5::new();
        if resume_effective {
            hash_file_prefix_into_hasher(&part_path_for_write, resume_offset, &mut hasher)?;
        }

        let mut stream = response.bytes_stream();
        let mut total_written = if resume_effective { resume_offset } else { 0 };
        let mut write_offset = total_written;
        loop {
            let next = compio::time::timeout(body_timeout, stream.next())
                .await
                .with_context(|| {
                    format!(
                        "Timed out reading response body from {} (timeout={}s)",
                        url_owned,
                        body_timeout.as_secs()
                    )
                })?;
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk.context("Failed to read response body chunk")?;
            hasher.update(chunk.as_ref());
            let chunk_len = chunk.len() as u64;
            let BufResult(write_result, _) = out.write_all_at(chunk, write_offset).await;
            write_result.with_context(|| {
                format!(
                    "Failed to append chunk to partial file {}",
                    part_path_for_write.display()
                )
            })?;
            write_offset = write_offset.saturating_add(chunk_len);
            total_written = total_written.saturating_add(chunk_len);
        }

        out.sync_data().await.with_context(|| {
            format!(
                "Failed to sync partial download file {}",
                part_path_for_write.display()
            )
        })?;

        if let Some(expected) = expected_size {
            if total_written != expected {
                anyhow::bail!(
                    "Downloaded size mismatch for {}: expected {}, got {}",
                    url_owned,
                    expected,
                    total_written
                );
            }
        }

        let actual_md5 = format!("{:x}", hasher.finalize());
        Ok::<(u64, String), anyhow::Error>((total_written, actual_md5))
    })?;

    if actual_md5 != expected_md5.to_lowercase() {
        anyhow::bail!(
            "MD5 mismatch: expected {}, got {}",
            expected_md5,
            actual_md5
        );
    }

    commit_partial_download(io_dispatcher, &part_path, dest)?;
    let dest_owned = dest.to_path_buf();
    let metadata = dispatch_io(io_dispatcher, move || async move {
        compio::fs::metadata(&dest_owned)
            .await
            .with_context(|| format!("Failed to stat {}", dest_owned.display()))
    })?;
    let len = metadata.len();
    if len != written {
        debug!(
            "download committed with metadata size {} differing from streamed size {} for {}",
            len, written, url
        );
    }
    Ok(len)
}

fn make_partial_download_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("Destination path has no parent")?;
    let file_name = path
        .file_name()
        .context("Destination path has no file name")?
        .to_string_lossy();
    let part_name = format!(".{}.griffr.part", file_name);

    Ok(parent.join(part_name))
}

fn hash_file_prefix_into_hasher(path: &Path, prefix_len: u64, hasher: &mut Md5) -> Result<()> {
    if prefix_len == 0 {
        return Ok(());
    }

    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut remaining = prefix_len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            anyhow::bail!(
                "Partial file shorter than expected prefix: {} < {} for {}",
                prefix_len - remaining,
                prefix_len,
                path.display()
            );
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

fn commit_partial_download(
    io_dispatcher: Option<&Dispatcher>,
    part_path: &Path,
    dest_path: &Path,
) -> Result<()> {
    let part_owned = part_path.to_path_buf();
    let dest_owned = dest_path.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        match compio::fs::metadata(&part_owned).await {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                anyhow::bail!("Missing partial download file {}", part_owned.display())
            }
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to stat {}", part_owned.display()))
            }
        }

        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }

        match compio::fs::metadata(&dest_owned).await {
            Ok(_) => {
                compio::fs::remove_file(&dest_owned)
                    .await
                    .with_context(|| format!("Failed to replace {}", dest_owned.display()))?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to stat {}", dest_owned.display()))
            }
        }

        compio::fs::rename(&part_owned, &dest_owned)
            .await
            .with_context(|| {
                format!(
                    "Failed to move {} to {}",
                    part_owned.display(),
                    dest_owned.display()
                )
            })?;

        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
fn write_file(io_dispatcher: Option<&Dispatcher>, path: &Path, bytes: Vec<u8>) -> Result<()> {
    let path_owned = path.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = path_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }

        let temp_path = make_temp_write_path(&path_owned)?;
        let write_res = compio::fs::write(&temp_path, bytes).await;
        if let Err(err) = write_res.0 {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(err)
                .with_context(|| format!("Failed to write temp file {}", temp_path.display()));
        }

        match compio::fs::metadata(&path_owned).await {
            Ok(_) => {
                compio::fs::remove_file(&path_owned)
                    .await
                    .with_context(|| format!("Failed to replace {}", path_owned.display()))?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                let _ = compio::fs::remove_file(&temp_path).await;
                return Err(err)
                    .with_context(|| format!("Failed to stat {}", path_owned.display()));
            }
        }

        if let Err(err) = compio::fs::rename(&temp_path, &path_owned).await {
            let _ = compio::fs::remove_file(&temp_path).await;
            return Err(err)
                .with_context(|| format!("Failed to move temp file to {}", path_owned.display()));
        }

        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
fn make_temp_write_path(path: &Path) -> Result<PathBuf> {
    static TEMP_WRITE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let parent = path.parent().context("Destination path has no parent")?;
    let file_name = path
        .file_name()
        .context("Destination path has no file name")?
        .to_string_lossy();
    let counter = TEMP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".{}.griffr.tmp.{}", file_name, counter);

    Ok(parent.join(temp_name))
}

fn make_extract_staging_dir(dest: &Path, base_name: &str) -> Result<PathBuf> {
    static EXTRACT_STAGING_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let counter = EXTRACT_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let staging_name = format!(".griffr.extract.{}.{}", base_name, counter);
    let parent = dest.parent().unwrap_or(dest);
    Ok(parent.join(staging_name))
}

fn commit_staged_extract(staging_root: &Path, dest_root: &Path) -> Result<()> {
    commit_staged_extract_inner(staging_root, staging_root, dest_root)?;
    std::fs::remove_dir_all(staging_root).with_context(|| {
        format!(
            "Failed to clean extraction staging directory {}",
            staging_root.display()
        )
    })?;
    Ok(())
}

fn commit_staged_extract_inner(
    staging_root: &Path,
    current: &Path,
    dest_root: &Path,
) -> Result<()> {
    for entry in std::fs::read_dir(current)
        .with_context(|| format!("Failed to read directory {}", current.display()))?
    {
        let entry = entry.with_context(|| {
            format!("Failed to read directory entry under {}", current.display())
        })?;
        let src_path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to inspect directory entry {}", src_path.display()))?;
        let relative = src_path.strip_prefix(staging_root).with_context(|| {
            format!(
                "Failed to derive relative path for staged entry {}",
                src_path.display()
            )
        })?;
        let dest_path = dest_root.join(relative);

        if file_type.is_dir() {
            std::fs::create_dir_all(&dest_path)
                .with_context(|| format!("Failed to create directory {}", dest_path.display()))?;
            commit_staged_extract_inner(staging_root, &src_path, dest_root)?;
            continue;
        }

        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }

        if dest_path.exists() {
            if dest_path.is_dir() {
                std::fs::remove_dir_all(&dest_path)
                    .with_context(|| format!("Failed to replace {}", dest_path.display()))?;
            }
        }

        move_path_replace(&src_path, &dest_path).with_context(|| {
            format!(
                "Failed to move extracted file {} -> {}",
                src_path.display(),
                dest_path.display()
            )
        })?;
    }

    Ok(())
}

fn move_path_replace(src: &Path, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        move_path_replace_windows(src, dest)
    }
    #[cfg(not(windows))]
    {
        if dest.exists() {
            if dest.is_dir() {
                std::fs::remove_dir_all(dest)
                    .with_context(|| format!("Failed to replace {}", dest.display()))?;
            } else {
                std::fs::remove_file(dest)
                    .with_context(|| format!("Failed to replace {}", dest.display()))?;
            }
        }
        std::fs::rename(src, dest).with_context(|| {
            format!(
                "Failed to rename staged path {} -> {}",
                src.display(),
                dest.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(windows)]
fn move_path_replace_windows(src: &Path, dest: &Path) -> Result<()> {
    let mut src_wide: Vec<u16> = src.as_os_str().encode_wide().collect();
    src_wide.push(0);
    let mut dest_wide: Vec<u16> = dest.as_os_str().encode_wide().collect();
    dest_wide.push(0);

    let moved = unsafe {
        MoveFileExW(
            src_wide.as_ptr(),
            dest_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "MoveFileExW failed to replace {} -> {}",
                src.display(),
                dest.display()
            )
        });
    }
    Ok(())
}

fn create_hardlink(io_dispatcher: Option<&Dispatcher>, src: &Path, dest: &Path) -> Result<()> {
    let src_owned = src.to_path_buf();
    let dest_owned = dest.to_path_buf();
    dispatch_io(io_dispatcher, move || async move {
        if let Some(parent) = dest_owned.parent() {
            compio::fs::create_dir_all(parent).await?;
        }
        if compio::fs::metadata(&dest_owned).await.is_ok() {
            let _ = compio::fs::remove_file(&dest_owned).await;
        }
        compio::fs::hard_link(&src_owned, &dest_owned)
            .await
            .with_context(|| {
                format!(
                    "Failed to hardlink {} -> {}",
                    src_owned.display(),
                    dest_owned.display()
                )
            })
    })?;
    Ok(())
}

enum ReuseMethod {
    Hardlink,
    Copy,
}

fn reuse_file(
    io_dispatcher: Option<&Dispatcher>,
    src: &Path,
    dest: &Path,
    allow_copy_fallback: bool,
) -> Result<ReuseMethod> {
    match create_hardlink(io_dispatcher, src, dest) {
        Ok(()) => Ok(ReuseMethod::Hardlink),
        Err(err) if allow_copy_fallback => {
            let dest_owned = dest.to_path_buf();
            dispatch_io(io_dispatcher, move || async move {
                if let Some(parent) = dest_owned.parent() {
                    compio::fs::create_dir_all(parent)
                        .await
                        .map_err(anyhow::Error::from)?;
                }
                match compio::fs::metadata(&dest_owned).await {
                    Ok(_) => {
                        let _ = compio::fs::remove_file(&dest_owned).await;
                    }
                    Err(meta_err) if meta_err.kind() == ErrorKind::NotFound => {}
                    Err(meta_err) => return Err(meta_err.into()),
                }
                Ok::<(), anyhow::Error>(())
            })?;

            std::fs::copy(src, dest).with_context(|| {
                format!("Failed to copy {} -> {}", src.display(), dest.display())
            })?;
            let dest_owned = dest.to_path_buf();
            let copied = dispatch_io(io_dispatcher, move || async move {
                compio::fs::metadata(&dest_owned)
                    .await
                    .map(|_| true)
                    .or_else(|meta_err| {
                        if meta_err.kind() == ErrorKind::NotFound {
                            Ok(false)
                        } else {
                            Err(meta_err)
                        }
                    })
                    .map_err(anyhow::Error::from)
            })?;
            if !copied {
                return Err(err);
            }
            Ok(ReuseMethod::Copy)
        }
        Err(err) => Err(err),
    }
}

fn dispatch_io<F, Fut, T>(io_dispatcher: Option<&Dispatcher>, task: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + 'static,
    T: Send + 'static,
{
    let dispatcher = io_dispatcher.context("IO dispatcher not available")?;
    let mut receiver = dispatcher
        .dispatch(task)
        .map_err(|_| anyhow::anyhow!("Failed to dispatch IO task"))?;

    loop {
        match receiver.try_recv() {
            Ok(Some(result)) => return result,
            Ok(None) => thread::sleep(Duration::from_millis(1)),
            Err(_) => anyhow::bail!("IO task cancelled"),
        }
    }
}

fn build_issue(
    path: &Path,
    logical_path: &str,
    expected_md5: &str,
    expected_size: Option<u64>,
) -> Option<FileIssue> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(0),
                actual_size: None,
                actual_md5: None,
                kind: FileIssueKind::Missing,
            });
        }
    };

    if let Some(expected_size) = expected_size {
        if metadata.len() != expected_size {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size,
                actual_size: Some(metadata.len()),
                actual_md5: None,
                kind: FileIssueKind::SizeMismatch,
            });
        }
    }

    let actual_md5 = match file_md5(path) {
        Ok(md5) => md5,
        Err(_) => {
            return Some(FileIssue {
                path: logical_path.to_string(),
                expected_md5: expected_md5.to_string(),
                expected_size: expected_size.unwrap_or(metadata.len()),
                actual_size: Some(metadata.len()),
                actual_md5: None,
                kind: FileIssueKind::Md5Mismatch,
            });
        }
    };
    if actual_md5 != expected_md5.to_lowercase() {
        return Some(FileIssue {
            path: logical_path.to_string(),
            expected_md5: expected_md5.to_string(),
            expected_size: expected_size.unwrap_or(metadata.len()),
            actual_size: Some(metadata.len()),
            actual_md5: Some(actual_md5),
            kind: FileIssueKind::Md5Mismatch,
        });
    }

    None
}

fn file_md5(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio::dispatcher::Dispatcher;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;
    use zip::write::FileOptions;

    #[test]
    fn test_make_temp_write_path_stays_in_parent_dir() {
        let target = PathBuf::from("target").join("Endfield.exe");
        let temp = make_temp_write_path(&target).unwrap();
        assert_eq!(temp.parent(), target.parent());
        let name = temp.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(".Endfield.exe.griffr.tmp."));
    }

    #[test]
    fn test_write_file_replaces_hardlink_instead_of_mutating_shared_inode() {
        let tmp = tempdir().unwrap();
        let original = tmp.path().join("original.bin");
        let linked = tmp.path().join("linked.bin");
        std::fs::write(&original, b"before").unwrap();
        std::fs::hard_link(&original, &linked).unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"before");
        assert_eq!(std::fs::read(&linked).unwrap(), b"before");

        let dispatcher = Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(1).unwrap())
            .build()
            .expect("dispatcher should build");
        write_file(Some(&dispatcher), &linked, b"after".to_vec()).unwrap();

        assert_eq!(std::fs::read(&linked).unwrap(), b"after");
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"before",
            "writing linked path must not mutate the original hardlinked file"
        );
    }

    #[test]
    fn ensure_file_can_relink_verified_target_when_prefer_reuse_enabled() {
        let tmp = tempdir().unwrap();
        let source = tmp.path().join("source.bin");
        let target = tmp.path().join("target.bin");
        std::fs::write(&source, b"same-bytes").unwrap();
        std::fs::write(&target, b"same-bytes").unwrap();
        let expected_md5 = format!("{:x}", Md5::digest(b"same-bytes"));

        let tasks = vec![Task::EnsureFile {
            dest: target.clone(),
            logical_path: "target.bin".to_string(),
            expected_md5,
            expected_size: 10,
            source_candidates: vec![source.clone()],
            download_url: None,
            allow_copy_fallback: false,
            prefer_reuse: true,
            retry_count: 0,
        }];

        let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ProgressEvent::Hardlinked { .. })),
            "expected hardlink event when prefer_reuse is enabled"
        );
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                ProgressEvent::Verified {
                    ok: true,
                    issue: None,
                    ..
                }
            )),
            "expected verify success after relink"
        );
    }

    fn start_test_http_server(
        routes: HashMap<String, Vec<u8>>,
    ) -> (String, Arc<Mutex<HashMap<String, usize>>>, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking test server");
        let addr = listener.local_addr().expect("server addr");
        let hits = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_thread = Arc::clone(&hits);
        let stop_thread = Arc::clone(&stop);

        thread::spawn(move || {
            while !stop_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        if read == 0 {
                            continue;
                        }
                        let req = String::from_utf8_lossy(&buf[..read]);
                        let first_line = req.lines().next().unwrap_or_default();
                        let path = first_line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();

                        {
                            let mut guard = hits_thread.lock().unwrap();
                            *guard.entry(path.clone()).or_insert(0) += 1;
                        }

                        if let Some(body) = routes.get(&path) {
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                        } else {
                            let body = b"not found";
                            let header = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        (format!("http://{}", addr), hits, stop)
    }

    fn start_range_http_server(
        path: &'static str,
        body: Vec<u8>,
    ) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking test server");
        let addr = listener.local_addr().expect("server addr");
        let range_hits = Arc::new(AtomicUsize::new(0));
        let total_hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let range_hits_thread = Arc::clone(&range_hits);
        let total_hits_thread = Arc::clone(&total_hits);
        let stop_thread = Arc::clone(&stop);

        thread::spawn(move || {
            while !stop_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 8192];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        if read == 0 {
                            continue;
                        }
                        total_hits_thread.fetch_add(1, Ordering::AcqRel);
                        let req = String::from_utf8_lossy(&buf[..read]);
                        let mut lines = req.lines();
                        let first_line = lines.next().unwrap_or_default();
                        let req_path = first_line.split_whitespace().nth(1).unwrap_or("/");
                        if req_path != path {
                            let body = b"not found";
                            let header = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                            continue;
                        }

                        let mut range_start = None::<usize>;
                        for line in lines {
                            let lower = line.to_ascii_lowercase();
                            if let Some(rest) = lower.strip_prefix("range: bytes=") {
                                if let Some((start, _end)) = rest.split_once('-') {
                                    if let Ok(parsed) = start.trim().parse::<usize>() {
                                        range_start = Some(parsed);
                                    }
                                }
                                break;
                            }
                        }

                        if let Some(start) = range_start {
                            range_hits_thread.fetch_add(1, Ordering::AcqRel);
                            let start = start.min(body.len());
                            let resp = &body[start..];
                            let content_range = format!(
                                "bytes {}-{}/{}",
                                start,
                                body.len().saturating_sub(1),
                                body.len()
                            );
                            let header = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: {}\r\nConnection: close\r\n\r\n",
                                resp.len(),
                                content_range
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(resp);
                        } else {
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(&body);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        (format!("http://{}", addr), range_hits, total_hits, stop)
    }

    #[test]
    fn do_download_resume_incremental_md5_produces_correct_result() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("asset.chk");
        let part = make_partial_download_path(&dest).unwrap();

        let mut payload = Vec::with_capacity(2 * 1024 * 1024 + 333);
        for i in 0..(2 * 1024 * 1024 + 333) {
            payload.push((i % 251) as u8);
        }
        let expected_md5 = format!("{:x}", Md5::digest(&payload));

        let cut = 1_048_576usize;
        std::fs::write(&part, &payload[..cut]).unwrap();

        let (base, range_hits, total_hits, stop) =
            start_range_http_server("/blob", payload.clone());
        let url = format!("{}/blob", base);

        let dispatcher = Dispatcher::builder()
            .worker_threads(NonZeroUsize::new(2).unwrap())
            .build()
            .expect("dispatcher should build");
        let len = do_download(
            Some(&dispatcher),
            "Mozilla/5.0",
            &url,
            &dest,
            &expected_md5,
            Some(payload.len() as u64),
        )
        .unwrap();

        stop.store(true, Ordering::Release);

        assert_eq!(len, payload.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        assert!(
            !part.exists(),
            "partial file should be promoted and removed after successful commit"
        );
        assert!(
            total_hits.load(Ordering::Acquire) >= 1,
            "expected at least one request"
        );
        assert!(
            range_hits.load(Ordering::Acquire) >= 1,
            "expected resume request with Range header"
        );
    }

    #[test]
    fn install_archive_recovers_from_interrupted_partial_part_on_rerun() {
        let tmp = tempdir().unwrap();
        let source_dir = tmp.path().join("downloads");
        let install_dir = tmp.path().join("install");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();

        let zip_path = tmp.path().join("bundle.zip");
        let zip_file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(zip_file);
        zip.start_file("data.txt", FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"recovered after interruption").unwrap();
        zip.finish().unwrap();

        let zip_bytes = std::fs::read(&zip_path).unwrap();
        let split_at = (zip_bytes.len() / 2).max(1);
        let part1 = zip_bytes[..split_at].to_vec();
        let part2 = zip_bytes[split_at..].to_vec();
        assert!(!part2.is_empty());

        let part1_path = source_dir.join("bundle.zip.001");
        let part2_path = source_dir.join("bundle.zip.002");
        // Simulate rerun after interruption:
        // - first part already complete
        // - second part partially written/corrupted
        std::fs::write(&part1_path, &part1).unwrap();
        std::fs::write(&part2_path, &part2[..(part2.len() / 2).max(1)]).unwrap();

        let mut routes = HashMap::new();
        routes.insert("/bundle.zip.001".to_string(), part1.clone());
        routes.insert("/bundle.zip.002".to_string(), part2.clone());
        let (base_url, hits, stop) = start_test_http_server(routes);

        let tasks = vec![Task::InstallArchive {
            source_dir: source_dir.clone(),
            base_name: "bundle".to_string(),
            dest: install_dir.clone(),
            cleanup: false,
            parts: vec![
                ArchivePart {
                    url: format!("{}/bundle.zip.001", base_url),
                    dest: part1_path.clone(),
                    logical_path: "bundle.zip.001".to_string(),
                    expected_md5: format!("{:x}", Md5::digest(&part1)),
                    expected_size: part1.len() as u64,
                },
                ArchivePart {
                    url: format!("{}/bundle.zip.002", base_url),
                    dest: part2_path.clone(),
                    logical_path: "bundle.zip.002".to_string(),
                    expected_md5: format!("{:x}", Md5::digest(&part2)),
                    expected_size: part2.len() as u64,
                },
            ],
        }];

        let mut cfg = TaskPoolConfig::default();
        cfg.max_retries = 1;
        let result = run_tasks(tasks, cfg).unwrap();
        stop.store(true, Ordering::Release);

        let downloaded = result
            .events
            .iter()
            .filter_map(|event| match event {
                ProgressEvent::Downloaded { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            downloaded,
            vec!["bundle.zip.002".to_string()],
            "rerun recovery should only redownload the corrupted partial part"
        );
        assert!(
            result
                .events
                .iter()
                .any(|event| matches!(event, ProgressEvent::Extracted { .. })),
            "archive should extract after recovering the missing/corrupt part"
        );

        let guard = hits.lock().unwrap();
        assert_eq!(
            guard.get("/bundle.zip.001").copied().unwrap_or(0),
            0,
            "valid completed part should be reused without HTTP download"
        );
        assert_eq!(
            guard.get("/bundle.zip.002").copied().unwrap_or(0),
            1,
            "corrupted partial part should be downloaded once"
        );

        let extracted = std::fs::read_to_string(install_dir.join("data.txt")).unwrap();
        assert_eq!(extracted, "recovered after interruption");
    }
}
