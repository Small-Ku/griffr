use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::types::GameFileEntry;
use crate::download::extractor::{
    ArchiveDirectory, ArchiveDirectoryDiscovery, ArchiveInspection, ArchiveRangeRequest,
    MultiVolumeExtractor, MultiVolumeLayout,
};
use crate::runtime::PatchApplyOptions;

use crate::runtime::task_pool::graph::{GraphExpansion, TaskExecution};
use crate::runtime::task_pool::types::{ArchiveRetention, ArchiveWork, Task, WorkerEvent};

pub(crate) async fn execute_fetch_archive_range(
    work: Arc<ArchiveWork>,
    request: ArchiveRangeRequest,
    retry_count: u32,
    max_retries: u32,
    progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskExecution {
    if work.layout.range_is_available(&request.global_range) {
        return TaskExecution::succeeded();
    }
    let logical_path = format!(
        "{}#volume-{:03}:{}-{}",
        work.base_name,
        request.volume_index + 1,
        request.local_range.start,
        request.local_range.end
    );
    let expected = request.local_range.end - request.local_range.start;
    let _ = event_tx.send(WorkerEvent::DownloadStarted {
        path: logical_path.clone(),
        total_bytes: expected,
    });

    let result = crate::download::extractor::fetch_archive_range_to_cache(
        &request,
        user_agent,
        progress_buffer_bytes,
        |written| {
            let _ = event_tx.send(WorkerEvent::DownloadedBytes {
                path: logical_path.clone(),
                bytes: written,
                total_bytes: expected,
            });
        },
    )
    .await
    .and_then(|written| {
        work.layout.register_range(&request)?;
        Ok(written)
    });

    match result {
        Ok(bytes) => {
            let _ = event_tx.send(WorkerEvent::DownloadedBytes {
                path: logical_path.clone(),
                bytes,
                total_bytes: expected,
            });
            let _ = event_tx.send(WorkerEvent::Downloaded {
                path: logical_path,
                bytes,
            });
            TaskExecution::succeeded()
        }
        Err(error) if retry_count < max_retries => {
            let _ = event_tx.send(WorkerEvent::Retried {
                path: logical_path,
                reason: format!("archive range attempt {} failed: {error}", retry_count + 1),
            });
            TaskExecution::then(Task::FetchArchiveRange {
                work,
                request,
                retry_count: retry_count + 1,
            })
        }
        Err(error) => TaskExecution::failed(format!(
            "archive range download failed after retries: {error}"
        )),
    }
}

pub(crate) fn execute_schedule_extract(
    base_name: String,
    volumes: Vec<PathBuf>,
    dest: PathBuf,
    retention: ArchiveRetention,
    password: Option<String>,
    patch_options: PatchApplyOptions,
    expected_files: Arc<std::collections::BTreeMap<String, GameFileEntry>>,
) -> TaskExecution {
    let layout = match MultiVolumeLayout::from_files(volumes) {
        Ok(layout) => layout,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    let work = match ArchiveWork::new(
        base_name,
        layout.clone(),
        vec![None; layout.volume_count()],
        dest,
        retention,
        Vec::new(),
        password,
        patch_options,
        expected_files,
    ) {
        Ok(work) => work,
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    TaskExecution::then(Task::DiscoverArchiveDirectory {
        work,
        required_range: None,
    })
}

fn fetch_ranges_then(
    work: Arc<ArchiveWork>,
    ranges: impl IntoIterator<Item = std::ops::Range<u64>>,
    next: Task,
) -> TaskExecution {
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
                return TaskExecution::failed(error.to_string());
            }
            let mut expansion = GraphExpansion::new();
            return match expansion.add_root_with_tokens(next, tokens) {
                Ok(_) => TaskExecution::expand(expansion),
                Err(error) => TaskExecution::failed(error.to_string()),
            };
        }
        Err(error) => return TaskExecution::failed(error.to_string()),
    };
    if requests.is_empty() {
        return TaskExecution::then(next);
    }
    let mut expansion = GraphExpansion::new();
    let mut fetches = Vec::with_capacity(requests.len());
    for request in requests {
        fetches.push(expansion.add_root(Task::FetchArchiveRange {
            work: work.clone(),
            request,
            retry_count: 0,
        }));
    }
    match expansion.add_task(next, fetches) {
        Ok(_) => TaskExecution::expand(expansion),
        Err(error) => TaskExecution::failed(error.to_string()),
    }
}

pub(crate) fn execute_discover_archive_directory(
    work: Arc<ArchiveWork>,
    required_range: Option<std::ops::Range<u64>>,
) -> TaskExecution {
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
        Err(error) => {
            work.invalidate_range_cache();
            TaskExecution::failed(error.to_string())
        }
    }
}

pub(crate) fn execute_inspect_archive_index(
    work: Arc<ArchiveWork>,
    directory: ArchiveDirectory,
) -> TaskExecution {
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.inspect_archive_index(&directory) {
        Ok(inspection) => {
            let inspection = Arc::new(inspection);
            let ranges = MultiVolumeExtractor::control_source_ranges(&inspection);
            fetch_ranges_then(
                work.clone(),
                ranges,
                Task::ReadArchiveControls { work, inspection },
            )
        }
        Err(error) => {
            work.invalidate_range_cache();
            TaskExecution::failed(error.to_string())
        }
    }
}

pub(crate) fn execute_read_archive_controls(
    work: Arc<ArchiveWork>,
    inspection: Arc<ArchiveInspection>,
) -> TaskExecution {
    let extractor = MultiVolumeExtractor::from_layout(work.layout.clone());
    match extractor.read_control_payloads(&inspection, work.password.as_deref()) {
        Ok(inspection) => TaskExecution::then(Task::PlanArchiveExtraction {
            work,
            inspection: Arc::new(inspection),
        }),
        Err(error) => {
            work.invalidate_range_cache();
            TaskExecution::failed(error.to_string())
        }
    }
}
