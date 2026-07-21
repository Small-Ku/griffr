use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::types::GameFileEntry;
use griffr_common::runtime::task_pool::{
    plan_archive_groups, ArchiveRetention, ArchiveSource, Task, TaskGraphBuilder, TaskOutcome,
    TaskPoolRunner, TaskProgress,
};
use griffr_common::runtime::{PatchApplyOptions, ProgressLane};

use super::*;
use crate::commands::archive_graph::{add_file_tasks, owned_archive_paths};
use crate::progress::ArchiveProgress;
use crate::ui;
use crate::GlobalOptions;

#[derive(Debug, Default)]
pub(super) struct ArchiveRunResult {
    pub(super) modified_paths: Vec<String>,
    pub(super) verified_paths: Vec<String>,
}

fn collect_archive_result(
    outcomes: Vec<TaskOutcome>,
    expected_files: &BTreeMap<String, GameFileEntry>,
    failure_label: &str,
) -> Result<ArchiveRunResult> {
    let mut modified_paths = BTreeSet::new();
    let mut verified_paths = BTreeSet::new();
    let mut failures = Vec::new();
    for event in outcomes {
        match event {
            TaskOutcome::Changed { path } => {
                modified_paths.insert(path);
            }
            TaskOutcome::Verified { path, ok: true, .. }
                if expected_files.contains_key(&path.replace('\\', "/").to_ascii_lowercase()) =>
            {
                verified_paths.insert(path);
            }
            TaskOutcome::Failed { path, reason } => {
                failures.push(format!("{} ({})", path, reason));
            }
            _ => {}
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "{failure_label} failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(ArchiveRunResult {
        modified_paths: modified_paths.into_iter().collect(),
        verified_paths: verified_paths.into_iter().collect(),
    })
}

#[allow(clippy::too_many_arguments)]
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
    extra_tasks: Vec<Task>,
    file_tasks_own_archive_paths: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<ArchiveRunResult> {
    let total_size: u64 = archives.iter().map(|pack| pack.size()).sum();
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
        let mut archive_nodes = Vec::with_capacity(archive_groups.len());
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
            archive_nodes.push(graph.add_task(
                Task::OpenArchive {
                    base_name: group.base_name.clone(),
                    source: ArchiveSource::Local(
                        group.parts.iter().map(|part| part.dest.clone()).collect(),
                    ),
                    dest: install_path.to_path_buf(),
                    retention: ArchiveRetention::from_keep_full_volumes(keep_pack_archives),
                    password: archive_password.map(str::to_owned),
                    patch_options: patch_options.clone(),
                    expected_files: expected_files.clone(),
                    excluded_commit_paths: Arc::new(BTreeSet::new()),
                },
                verify_nodes,
            )?);
        }
        let extra_task_count = extra_tasks.len();
        let (parallel_vfs, dependent_vfs) = add_file_tasks(
            &mut graph,
            extra_tasks,
            &archive_nodes,
            install_path,
            expected_files.as_ref(),
            true,
        )?;
        opts.verbose(format!(
            "VFS/archive ownership: {parallel_vfs} independent task(s), {dependent_vfs} patch-dependent task(s)"
        ));

        let progress = ArchiveProgress::new(&format!("update.{label}.apply"), opts.verbose);
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
            .with_verify(
                verify_lane,
                verify_task_count.saturating_add(extra_task_count),
            )
            .with_download(download_lane)
            .with_extract(extract_lane)
            .with_commit(commit_lane)
            .with_patch(patch_lane)
            .with_delete(delete_lane);
        let result = task_pool_runner.run_graph(graph.build_checked()?, task_progress)?;
        progress_session.finish();
        progress.finish();

        for outcome in &result.outcomes {
            if let TaskOutcome::ArchiveCheck { report, .. } = outcome {
                ui::print_patch_check(report);
            }
        }
        return collect_archive_result(
            result.outcomes,
            expected_files.as_ref(),
            "Predownload archive DAG",
        );
    }

    let archive_group_count = archive_groups.len();
    let archive_verify_count = if keep_pack_archives {
        archives.len()
    } else {
        archive_group_count
    };
    let excluded_commit_paths = if file_tasks_own_archive_paths {
        owned_archive_paths(&extra_tasks, install_path, expected_files.as_ref())
    } else {
        Arc::new(BTreeSet::new())
    };
    let mut graph = TaskGraphBuilder::new();
    let mut archive_nodes = Vec::with_capacity(archive_group_count);
    for group in archive_groups {
        opts.verbose(format!("queued archive state-machine {}", group.base_name));
        archive_nodes.push(graph.add_root(Task::OpenArchive {
            base_name: group.base_name,
            source: ArchiveSource::Remote(group.parts),
            dest: install_path.to_path_buf(),
            retention: ArchiveRetention::from_keep_full_volumes(keep_pack_archives),
            password: archive_password.map(str::to_owned),
            patch_options: patch_options.clone(),
            expected_files: expected_files.clone(),
            excluded_commit_paths: excluded_commit_paths.clone(),
        }));
    }
    let extra_task_count = extra_tasks.len();
    let (parallel_vfs, dependent_vfs) = add_file_tasks(
        &mut graph,
        extra_tasks,
        &archive_nodes,
        install_path,
        expected_files.as_ref(),
        !file_tasks_own_archive_paths,
    )?;
    opts.verbose(format!(
        "VFS/archive ownership: {parallel_vfs} independent task(s), {dependent_vfs} archive-dependent task(s)"
    ));

    let progress = ArchiveProgress::new(&format!("update.{label}"), opts.verbose);
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
    let destination_verify_count = if file_tasks_own_archive_paths {
        expected_files
            .len()
            .saturating_sub(excluded_commit_paths.len())
    } else {
        0
    };
    let task_progress = TaskProgress::new(progress_session.sender())
        .with_verify(
            verify_lane,
            archive_verify_count
                .saturating_add(destination_verify_count)
                .saturating_add(extra_task_count),
        )
        .with_download(download_lane)
        .with_extract(extract_lane)
        .with_commit(commit_lane)
        .with_patch(patch_lane)
        .with_delete(delete_lane);
    let result = task_pool_runner.run_graph(graph.build_checked()?, task_progress)?;
    progress_session.finish();
    progress.finish();

    for outcome in &result.outcomes {
        if let TaskOutcome::ArchiveCheck { report, .. } = outcome {
            ui::print_patch_check(report);
        }
    }
    collect_archive_result(
        result.outcomes,
        expected_files.as_ref(),
        "Update archive work",
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    keep_pack_archives: bool,
    archive_password: Option<&str>,
    patch_options: &PatchApplyOptions,
    expected_files: Arc<BTreeMap<String, GameFileEntry>>,
    extra_tasks: Vec<Task>,
    file_tasks_own_archive_paths: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<ArchiveRunResult> {
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
        extra_tasks,
        file_tasks_own_archive_paths,
        opts,
        task_pool_runner,
    )
    .await
}

pub(super) async fn validate_patch_target(exe_name: &Path, install_path: &Path) -> Result<()> {
    let expected_exe = install_path.join(exe_name);
    match compio::fs::metadata(&expected_exe).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Patch target missing {}", expected_exe.display());
        }
        Err(error) => Err(error)
            .with_context(|| format!("Failed to stat patch target {}", expected_exe.display())),
    }
}
