use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::download::extractor::{ArchiveIndex, MultiVolumeExtractor};
use crate::error::{Error, Result};
use crate::runtime::task_pool::fs_ops::extract::CommitFileJob;
use crate::runtime::task_pool::fs_ops::{commit_file_job, make_extract_staging_dir};
use crate::runtime::task_pool::graph::TaskRun;
use crate::runtime::task_pool::types::{
    ArchiveFileRepairTask, ArchiveRepairSession, ArchiveWork, PreparedArchiveRepairGroup, Task,
    WorkerEvent,
};
use crate::runtime::task_pool::verify::build_issue;
use crate::runtime::PatchApplyOptions;

use super::install::prepare_remote_archive_range_work;

pub(crate) fn start_archive_repair_index(session: Arc<ArchiveRepairSession>) -> Vec<Task> {
    if !session.try_start_prepare() {
        return Vec::new();
    }

    let download_dir = session.install_root().join("downloads");
    if let Err(error) = std::fs::create_dir_all(&download_dir) {
        for group_index in 0..session.group_specs().len() {
            session.set_group_failed(group_index);
        }
        tracing::warn!(
            path = %download_dir.display(),
            %error,
            "archive repair index is unavailable; direct repair remains active"
        );
        return Vec::new();
    }

    session
        .group_specs()
        .iter()
        .enumerate()
        .filter_map(|(group_index, spec)| {
            match prepare_remote_archive_range_work(
                spec.base_name.clone(),
                spec.parts.clone(),
                session.install_root().to_path_buf(),
                None,
                PatchApplyOptions::default(),
                session.expected_files().clone(),
                Arc::new(BTreeSet::new()),
                Arc::downgrade(&session),
                group_index,
            ) {
                Ok(work) => Some(Task::DiscoverArchiveDirectory {
                    work,
                    required_range: None,
                }),
                Err(error) => {
                    session.set_group_failed(group_index);
                    tracing::warn!(
                        archive = %spec.base_name,
                        %error,
                        "archive repair index is unavailable; direct repair remains active"
                    );
                    None
                }
            }
        })
        .collect()
}

pub(super) fn finish_archive_repair_index(
    work: Arc<ArchiveWork>,
    archive_index: Arc<ArchiveIndex>,
) -> Result<()> {
    let Some((session, group_index)) = work.repair_target() else {
        return Err(Error::Message {
            context: "Task pool error: ",
            detail: format!("archive {} is not repair metadata work", work.base_name),
        });
    };
    let Some(session) = session.upgrade() else {
        work.invalidate_range_cache();
        return Ok(());
    };
    let staging_dir = make_extract_staging_dir(
        session.install_root(),
        &format!("repair-{}", work.base_name),
        None,
    )?;
    std::fs::create_dir_all(&staging_dir).map_err(|source| Error::IoAt {
        action: "create directory",
        path: staging_dir.clone(),
        source,
    })?;
    let group = PreparedArchiveRepairGroup {
        work: work.clone(),
        archive_index,
        staging_dir: staging_dir.clone(),
    };
    if !session.set_group_ready(group_index, group) {
        let _ = std::fs::remove_dir_all(staging_dir);
        work.invalidate_range_cache();
    }
    Ok(())
}

fn direct_fallback(
    repair: ArchiveFileRepairTask,
    reason: impl std::fmt::Display,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let _ = event_tx.send(WorkerEvent::Retried {
        path: repair.logical_path.clone(),
        reason: format!("archive repair failed; using individual file download: {reason}"),
    });
    let Some(url) = repair.download_url else {
        return TaskRun::failed(format!(
            "archive repair failed for {} and no individual download URL is available: {reason}",
            repair.logical_path
        ));
    };
    TaskRun::then(Task::Download {
        url,
        dest: repair.dest,
        logical_path: repair.logical_path,
        expected_md5: repair.expected_md5,
        expected_size: Some(repair.expected_size),
        retry_count: repair.retry_count,
        transfer_class: repair.transfer_class,
        archive_repair: None,
        resume: None,
    })
}

pub(crate) async fn run_fetch_archive_repair_file(
    mut repair: ArchiveFileRepairTask,
    max_retries: u32,
    progress_buffer_bytes: usize,
    user_agent: &str,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let requests = match repair
        .work
        .layout
        .missing_range_requests([repair.source_range.clone()])
    {
        Ok(requests) => requests,
        Err(error) => return direct_fallback(repair, error, event_tx),
    };
    let mut downloaded = 0u64;
    for request in requests {
        let logical_path = format!(
            "{}#volume-{:03}:{}-{}",
            repair.work.base_name,
            request.volume_index + 1,
            request.local_range.start,
            request.local_range.end
        );
        let mut fetched = None;
        for attempt in 0..=max_retries {
            match super::fetch::fetch_archive_range_once(
                &repair.work,
                &request,
                progress_buffer_bytes,
                user_agent,
                event_tx,
            )
            .await
            {
                Ok(bytes) => {
                    fetched = Some(bytes);
                    break;
                }
                Err(error) if attempt < max_retries => {
                    let _ = event_tx.send(WorkerEvent::Retried {
                        path: logical_path.clone(),
                        reason: format!("archive range attempt {} failed: {error}", attempt + 1),
                    });
                }
                Err(error) => {
                    repair.work.invalidate_range_cache();
                    return direct_fallback(repair, error, event_tx);
                }
            }
        }
        downloaded = downloaded
            .saturating_add(fetched.expect("archive range retry loop always returns or succeeds"));
    }
    repair.source_bytes = downloaded;
    TaskRun::then(Task::ExtractArchiveRepairFile { repair })
}

pub(crate) fn run_extract_archive_repair_file(
    repair: ArchiveFileRepairTask,
    progress_buffer_bytes: usize,
    event_tx: &flume::Sender<WorkerEvent>,
) -> TaskRun {
    let extractor = MultiVolumeExtractor::from_layout(repair.work.layout.clone());
    let result = extractor.extract_entries_with_progress(
        &repair.staging_dir,
        None,
        &repair.archive_index,
        &[repair.entry_index],
        &repair.work.expected_files,
        progress_buffer_bytes,
        |_| {},
    );
    if let Err(error) = result {
        repair.work.invalidate_range_cache();
        return direct_fallback(repair, error, event_tx);
    }

    let archive_name = match repair
        .archive_index
        .archive
        .name_for_index(repair.entry_index)
    {
        Some(name) => name,
        None => return direct_fallback(repair, "archive entry name is unavailable", event_tx),
    };
    let relative = match crate::download::extractor::safe_relative_archive_path(archive_name) {
        Ok(path) => path,
        Err(error) => return direct_fallback(repair, error, event_tx),
    };
    let source = repair.staging_dir.join(&relative);
    let job = CommitFileJob {
        source: source.clone(),
        destination: repair.dest.clone(),
        logical_path: PathBuf::from(&repair.logical_path),
    };
    if let Err(error) = commit_file_job(&job) {
        let _ = std::fs::remove_file(source);
        return direct_fallback(repair, error, event_tx);
    }
    if let Some(issue) = build_issue(
        &repair.dest,
        &repair.logical_path,
        &repair.expected_md5,
        Some(repair.expected_size),
    ) {
        return direct_fallback(
            repair,
            format!(
                "committed archive file failed verification: {:?}",
                issue.kind
            ),
            event_tx,
        );
    }

    if let Ok(part_path) =
        crate::runtime::task_pool::fs_ops::make_partial_download_path(&repair.dest)
    {
        if let Err(error) = std::fs::remove_file(&part_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %part_path.display(),
                    %error,
                    "failed to remove stale individual-repair partial after archive repair"
                );
            }
        }
    }

    let _ = event_tx.send(WorkerEvent::downloaded(
        repair.logical_path.clone(),
        repair.source_bytes,
    ));
    let _ = event_tx.send(WorkerEvent::changed(repair.logical_path.clone()));
    let _ = event_tx.send(WorkerEvent::verified(repair.logical_path, true, None));
    TaskRun::succeeded()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::runtime::task_pool::types::{
        ArchivePart, ArchiveRepairGroupSpec, ArchiveRepairSession, Task,
    };

    use super::start_archive_repair_index;

    #[test]
    fn repair_metadata_reuses_archive_discovery_and_starts_once() {
        let temp = tempfile::tempdir().unwrap();
        let install_root = temp.path().join("game");
        let session = ArchiveRepairSession::new(
            vec![ArchiveRepairGroupSpec {
                base_name: "bundle".to_string(),
                parts: vec![ArchivePart {
                    sequence: 1,
                    url: "https://example.invalid/bundle.zip.001".to_string(),
                    dest: install_root.join("downloads/bundle.zip.001"),
                    logical_path: "bundle.zip.001".to_string(),
                    expected_md5: "00".repeat(16),
                    expected_size: 16,
                }],
            }],
            install_root,
            Arc::new(BTreeMap::new()),
        );

        let first = start_archive_repair_index(session.clone());
        assert!(matches!(
            first.as_slice(),
            [Task::DiscoverArchiveDirectory {
                required_range: None,
                ..
            }]
        ));
        assert!(start_archive_repair_index(session).is_empty());
    }
}
