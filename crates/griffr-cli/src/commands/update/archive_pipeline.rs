use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use griffr_common::runtime::task_pool::{
    plan_archive_groups, Task, TaskOutcome, TaskPoolRunner, TaskProgress,
};

use super::*;
use crate::progress::{ArchivePipelineProgress, StepProgress};
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::{ProgressLane, ProgressUnit};

pub(super) async fn download_and_extract_archives_from_dir(
    archives: &[griffr_common::api::types::PackFile],
    archive_dir: &Path,
    install_path: &Path,
    label: &str,
    keep_pack_archives: bool,
    archive_password: Option<&str>,
    mode: ArchiveAcquireMode,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
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
        let mut verify_tasks = Vec::new();
        let mut extract_tasks = Vec::new();
        for group in &archive_groups {
            opts.verbose(format!(
                "queued predownload apply archive {}",
                group.base_name
            ));
            for part in &group.parts {
                verify_tasks.push(Task::Verify {
                    path: part.dest.clone(),
                    logical_path: part.logical_path.clone(),
                    expected_md5: part.expected_md5.clone(),
                    expected_size: Some(part.expected_size),
                    on_fail: None,
                });
            }
            extract_tasks.push(Task::Extract {
                base_name: group.base_name.clone(),
                volumes: group.parts.iter().map(|part| part.dest.clone()).collect(),
                dest: install_path.to_path_buf(),
                cleanup: !keep_pack_archives,
                password: archive_password.map(str::to_owned),
            });
        }

        let verify_task_count = verify_tasks.len();
        let verify_bar =
            StepProgress::new(format!("update.{}.archive-verify", label), opts.verbose);
        let verify_lane = ProgressLane::ARCHIVE_VERIFY;
        let verify_session = verify_bar.start(verify_lane, ProgressUnit::Items);
        let verify_result = task_pool_runner.run_batch(
            verify_tasks,
            TaskProgress::new(verify_session.sender()).with_verify(verify_lane, verify_task_count),
        )?;
        verify_session.finish();
        verify_bar.finish();
        let verify_failures = verify_result
            .outcomes
            .into_iter()
            .filter_map(|event| match event {
                TaskOutcome::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !verify_failures.is_empty() {
            anyhow::bail!(
                "Predownload apply requires complete staged archives; missing/mismatched items: {}",
                verify_failures.join(", ")
            );
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
            .with_extract(extract_lane)
            .with_commit(commit_lane)
            .with_patch(patch_lane)
            .with_delete(delete_lane);
        let result = task_pool_runner.run_batch(extract_tasks, task_progress)?;
        progress_session.finish();
        progress.finish();

        let failures = result
            .outcomes
            .into_iter()
            .filter_map(|event| match event {
                TaskOutcome::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            anyhow::bail!(
                "Archive apply failed for {} item(s): {}",
                failures.len(),
                failures.join(", ")
            );
        }
        return Ok(());
    }

    let mut tasks = Vec::with_capacity(archive_groups.len());
    for group in archive_groups {
        opts.verbose(format!("queued archive state-machine {}", group.base_name));
        tasks.push(Task::InstallArchive {
            base_name: group.base_name,
            dest: install_path.to_path_buf(),
            cleanup: !keep_pack_archives,
            password: archive_password.map(str::to_owned),
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
        .with_verify(verify_lane, archives.len())
        .with_download(download_lane)
        .with_extract(extract_lane)
        .with_commit(commit_lane)
        .with_patch(patch_lane)
        .with_delete(delete_lane);
    let result = task_pool_runner.run_batch(tasks, task_progress)?;
    progress_session.finish();
    progress.finish();

    let mut failures = Vec::new();
    for event in result.outcomes {
        if let TaskOutcome::Failed { path, reason } = event {
            failures.push(format!("{} ({})", path, reason));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Update archive pipeline failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    Ok(())
}

pub(super) async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    keep_pack_archives: bool,
    archive_password: Option<&str>,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    let download_dir = install_path.join("downloads");
    download_and_extract_archives_from_dir(
        archives,
        &download_dir,
        install_path,
        label,
        keep_pack_archives,
        archive_password,
        ArchiveAcquireMode::DownloadIfMissing,
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
