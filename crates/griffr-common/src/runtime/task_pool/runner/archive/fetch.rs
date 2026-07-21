use std::collections::BTreeSet;
use std::sync::Arc;

use crate::download::extractor::{
    ArchiveDirectory, ArchiveDirectoryDiscovery, ArchiveIndex, ArchiveRangeRequest,
    MultiVolumeExtractor,
};
use crate::runtime::task_pool::graph::{GraphExpansion, TaskRun};
use crate::runtime::task_pool::types::{ArchiveRangePriority, ArchiveWork, Task, WorkerEvent};

fn archive_range_logical_path(work: &ArchiveWork, request: &ArchiveRangeRequest) -> String {
    format!(
        "{}#volume-{:03}:{}-{}",
        work.base_name,
        request.volume_index + 1,
        request.local_range.start,
        request.local_range.end
    )
}

pub(super) async fn fetch_archive_range_once(
    work: &Arc<ArchiveWork>,
    request: &ArchiveRangeRequest,
    progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> crate::error::Result<u64> {
    if work.layout.range_is_available(&request.global_range) {
        return Ok(0);
    }
    let logical_path = archive_range_logical_path(work, request);
    let expected = request.local_range.end - request.local_range.start;
    let _ = event_tx.send(WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        logical_path.clone(),
        0,
        expected,
        false,
    ));
    let written = crate::download::extractor::fetch_archive_range_to_cache(
        request,
        user_agent,
        progress_buffer_bytes,
        |written| {
            let _ = event_tx.send(WorkerEvent::progress(
                crate::runtime::ProgressPhase::Download,
                logical_path.clone(),
                written,
                expected,
                false,
            ));
        },
    )
    .await?;
    work.layout.register_range(request)?;
    let _ = event_tx.send(WorkerEvent::progress(
        crate::runtime::ProgressPhase::Download,
        logical_path.clone(),
        written,
        expected,
        false,
    ));
    Ok(written)
}

pub(crate) async fn run_fetch_archive_range(
    work: Arc<ArchiveWork>,
    request: ArchiveRangeRequest,
    retry_count: u32,
    priority: ArchiveRangePriority,
    max_retries: u32,
    progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    if work.repair_index_stopped() {
        return TaskRun::succeeded();
    }
    let logical_path = archive_range_logical_path(&work, &request);
    match fetch_archive_range_once(&work, &request, progress_buffer_bytes, user_agent, event_tx)
        .await
    {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::downloaded(logical_path, bytes));
            TaskRun::succeeded()
        }
        Err(error) if retry_count < max_retries => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path,
                reason: format!("archive range attempt {} failed: {error}", retry_count + 1),
            });
            TaskRun::then(Task::FetchArchiveRange {
                work,
                request,
                retry_count: retry_count + 1,
                priority,
            })
        }
        Err(error) => {
            let detail = format!("archive range download failed after retries: {error}");
            if work.fail_repair_index(&detail) {
                TaskRun::succeeded()
            } else {
                TaskRun::failed(detail)
            }
        }
    }
}

fn fail_archive_work(work: &Arc<ArchiveWork>, error: impl std::fmt::Display) -> TaskRun {
    let detail = error.to_string();
    if work.fail_repair_index(&detail) {
        TaskRun::succeeded()
    } else {
        work.invalidate_range_cache();
        TaskRun::failed(detail)
    }
}

fn fetch_ranges_then(
    work: Arc<ArchiveWork>,
    ranges: impl IntoIterator<Item = std::ops::Range<u64>>,
    next: Task,
) -> TaskRun {
    let ranges = ranges.into_iter().collect::<Vec<_>>();
    let requests = match work.layout.missing_range_requests(ranges.clone()) {
        Ok(requests) => requests,
        Err(error) if !work.layout.is_remote() => {
            let tokens = ranges
                .iter()
                .flat_map(|range| work.tokens_for_range(range.clone()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if tokens.is_empty() {
                return fail_archive_work(&work, error);
            }
            let mut expansion = GraphExpansion::new();
            return match expansion.add_root_with_tokens(next, tokens) {
                Ok(_) => TaskRun::expand(expansion),
                Err(error) => fail_archive_work(&work, error),
            };
        }
        Err(error) => return fail_archive_work(&work, error),
    };
    if requests.is_empty() {
        return TaskRun::then(next);
    }
    let mut expansion = GraphExpansion::new();
    for request in requests {
        expansion.add_root(Task::FetchArchiveRange {
            work: work.clone(),
            request,
            retry_count: 0,
            priority: ArchiveRangePriority::ExtractionCritical,
        });
    }
    TaskRun::expand_then(expansion, next)
}

pub(crate) fn run_discover_archive_directory(
    work: Arc<ArchiveWork>,
    required_range: Option<std::ops::Range<u64>>,
) -> TaskRun {
    if work.repair_index_stopped() {
        return TaskRun::succeeded();
    }
    if let Some(range) = required_range.as_ref() {
        if !work.layout.range_is_available(range) {
            return fetch_ranges_then(
                work.clone(),
                [range.clone()],
                Task::DiscoverArchiveDirectory {
                    work,
                    required_range: Some(range.clone()),
                },
            );
        }
    }
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.discover_archive_directory() {
        Ok(ArchiveDirectoryDiscovery::Ready(directory)) => fetch_ranges_then(
            work.clone(),
            [
                directory.central_directory.clone(),
                directory.end_records.clone(),
            ],
            Task::InspectArchiveIndex { work, directory },
        ),
        Ok(ArchiveDirectoryDiscovery::NeedsRange(range)) => fetch_ranges_then(
            work.clone(),
            [range.clone()],
            Task::DiscoverArchiveDirectory {
                work,
                required_range: Some(range),
            },
        ),
        Err(error) => fail_archive_work(&work, error),
    }
}

pub(crate) fn run_read_archive_index(
    work: Arc<ArchiveWork>,
    directory: ArchiveDirectory,
) -> TaskRun {
    if work.repair_index_stopped() {
        return TaskRun::succeeded();
    }
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.read_archive_index(&directory) {
        Ok(archive_index) => {
            let archive_index = Arc::new(archive_index);
            if work.repair_target().is_some() {
                return match super::repair::finish_archive_repair_index(work.clone(), archive_index)
                {
                    Ok(()) => TaskRun::succeeded(),
                    Err(error) => fail_archive_work(&work, error),
                };
            }
            let ranges = MultiVolumeExtractor::control_source_ranges(&archive_index);
            fetch_ranges_then(
                work.clone(),
                ranges,
                Task::ReadArchiveControls {
                    work,
                    archive_index,
                },
            )
        }
        Err(error) => fail_archive_work(&work, error),
    }
}

pub(crate) fn run_read_archive_controls(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
    extract_shards: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.read_control_payloads(&archive_index, work.password.as_deref()) {
        Ok(archive_index) => super::extract::run_plan_archive_extraction(
            work,
            Arc::new(archive_index),
            extract_shards,
            event_tx,
        ),
        Err(error) => fail_archive_work(&work, error),
    }
}
