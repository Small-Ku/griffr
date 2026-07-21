use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::types::GameFileEntry;
use griffr_common::runtime::task_pool::{
    plan_archive_groups, ArchiveRetention, Task, TaskGraphBuilder, TaskOutcome, TaskPoolRunner,
    TaskProgress,
};

use super::*;
use crate::progress::ArchivePipelineProgress;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::{PatchApplyOptions, ProgressLane};

pub(super) async fn download_and_extract_archives_from_dir(
    archives: &[griffr_common::api::types::PackFile],
    archive_dir: &Path,
    install_path: &Path,
    label: &str,
    keep_pack_archives: bool,
    archive_password: Option<&str>,
    mode: ArchiveAcquireMode,
    patch_options: &PatchApplyOptions,
    expected_files: Arc<BTreeMap<String, GameFileEntry>>,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<Vec<String>> {
    let total_size: u64 = archives.iter().map(|p| p.size()).sum();
    let phase_verb = match mode {
        ArchiveAcquireMode::DownloadIfMissing => "Downloading",
        ArchiveAcquireMode::RequireExisting => "Applying",
    };
    ui::print_phase(format!(
        "{phase_verb} {label} package archives ({})",
        ui::format_bytes(total_size)
    ));

    compio::fs::create_dir_all(archive_dir)
        .await
        .with_context(|| format!("Failed to create {}", archive_dir.display()))?;

    let archive_groups = plan_archive_groups(archives, archive_dir)?;

    if mode == ArchiveAcquireMode::RequireExisting {
        let mut graph = TaskGraphBuilder::new();
        let mut verify_task_count = 0usize;
        for group in &archive_groups {
            opts.verbose(format!(
                "queued predownload apply archive {}",
                group.base_name
            ));
            let verify_nodes = group
                .parts
                .iter()
                .map(|part| {
                    verify_task_count = verify_task_count.saturating_add(1);
                    graph.add_root(Task::Verify {
                        path: part.dest.clone(),
                        logical_path: part.logical_path.clone(),
                        expected_md5: part.expected_md5.clone(),
                        expected_size: Some(part.expected_size),
                        on_fail: None,
                    })
                })
                .collect::<Vec<_>>();
            graph.add_task(
                Task::Extract {
                    base_name: group.base_name.clone(),
                    volumes: group.parts.iter().map(|part| part.dest.clone()).collect(),
                    dest: install_path.to_path_buf(),
                    retention: ArchiveRetention::from_keep_complete_volumes(keep_pack_archives),
                    password: archive_password.map(str::to_owned),
                    patch_options: patch_options.clone(),
                    expected_files: expected_files.clone(),
                },
                verify_nodes,
            )?;
        }

        let progress = ArchivePipelineProgress::new(&format!("update.{label}.apply"), opts.verbose);
        let verify_lane = ProgressLane::ARCHIVE_VERIFY;
        let download_lane = ProgressLane::ARCHIVE_DOWNLOAD;
        let extract_lane = ProgressLane::ARCHIVE_EXTRACT;
        let commit_lane = ProgressLane::ARCHIVE_COMMIT;
        let patch_lane = ProgressLane::ARCHIVE_PATCH;
        let delete_lane = ProgressLane::ARCHIVE_DELETE;
        let progress_session = progress.start(
            verify_lane,
            download_lane,
            extract_lane,
            commit_lane,
            patch_lane,
            delete_lane,
        );
        let task_progress = TaskProgress::new(progress_session.sender())
            .with_verify(verify_lane, verify_task_count)
            .with_extract(extract_lane)
            .with_commit(commit_lane)
            .with_patch(patch_lane)
            .with_delete(delete_lane);
        let result = task_pool_runner.run_graph(graph.build_checked()?, task_progress)?;
        progress_session.finish();
        progress.finish();

        for outcome in &result.outcomes {
            if let TaskOutcome::ArchivePreflight { report, .. } = outcome {
                ui::print_patch_preflight(report);
            }
        }

        let mut modified_paths = BTreeSet::new();
        let mut failures = Vec::new();
        for event in result.outcomes {
            match event {
                TaskOutcome::Changed { path } => {
                    modified_paths.insert(path);
                }
                TaskOutcome::Failed { path, reason } => {
                    failures.push(format!("{} ({})", path, reason));
                }
                _ => {}
            }
        }
        if !failures.is_empty() {
            anyhow::bail!(
                "Predownload archive DAG failed for {} item(s): {}",
                failures.len(),
                failures.join(", ")
            );
        }
        return Ok(modified_paths.into_iter().collect());
    }

    let archive_group_count = archive_groups.len();
    let archive_verify_count = if keep_pack_archives {
        archives.len()
    } else {
        archive_group_count
    };
    let mut tasks = Vec::with_capacity(archive_group_count);
    for group in archive_groups {
        opts.verbose(format!("queued archive state-machine {}", group.base_name));
        tasks.push(Task::InstallArchive {
            base_name: group.base_name,
            dest: install_path.to_path_buf(),
            retention: ArchiveRetention::from_keep_complete_volumes(keep_pack_archives),
            password: archive_password.map(str::to_owned),
            patch_options: patch_options.clone(),
            expected_files: expected_files.clone(),
            parts: group.parts,
        });
    }

    let progress = ArchivePipelineProgress::new(&format!("update.{label}"), opts.verbose);
    let verify_lane = ProgressLane::ARCHIVE_VERIFY;
    let download_lane = ProgressLane::ARCHIVE_DOWNLOAD;
    let extract_lane = ProgressLane::ARCHIVE_EXTRACT;
    let commit_lane = ProgressLane::ARCHIVE_COMMIT;
    let patch_lane = ProgressLane::ARCHIVE_PATCH;
    let delete_lane = ProgressLane::ARCHIVE_DELETE;
    let progress_session = progress.start(
        verify_lane,
        download_lane,
        extract_lane,
        commit_lane,
        patch_lane,
        delete_lane,
    );
    let task_progress = TaskProgress::new(progress_session.sender())
        .with_verify(verify_lane, archive_verify_count)
        .with_download(download_lane)
        .with_extract(extract_lane)
        .with_commit(commit_lane)
        .with_patch(patch_lane)
        .with_delete(delete_lane);
    let result = task_pool_runner.run_batch(tasks, task_progress)?;
    progress_session.finish();
    progress.finish();

    for outcome in &result.outcomes {
        if let TaskOutcome::ArchivePreflight { report, .. } = outcome {
            ui::print_patch_preflight(report);
        }
    }

    let mut modified_paths = BTreeSet::new();
    let mut failures = Vec::new();
    for event in result.outcomes {
        match event {
            TaskOutcome::Changed { path } => {
                modified_paths.insert(path);
            }
            TaskOutcome::Failed { path, reason } => {
                failures.push(format!("{} ({})", path, reason));
            }
            _ => {}
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Update archive pipeline failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    Ok(modified_paths.into_iter().collect())
}

pub(super) async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    keep_pack_archives: bool,
    archive_password: Option<&str>,
    patch_options: &PatchApplyOptions,
    expected_files: Arc<BTreeMap<String, GameFileEntry>>,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<Vec<String>> {
    let download_dir = install_path.join("downloads");
    download_and_extract_archives_from_dir(
        archives,
        &download_dir,
        install_path,
        label,
        keep_pack_archives,
        archive_password,
        ArchiveAcquireMode::DownloadIfMissing,
        patch_options,
        expected_files,
        opts,
        task_pool_runner,
    )
    .await
}

pub(super) async fn validate_patch_target(executable: &Path, install_path: &Path) -> Result<()> {
    let expected_exe = install_path.join(executable);
    match compio::fs::metadata(&expected_exe).await {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Patch target missing {}", expected_exe.display());
        }
        Err(err) => Err(err)
            .with_context(|| format!("Failed to stat patch target {}", expected_exe.display())),
    }
}
