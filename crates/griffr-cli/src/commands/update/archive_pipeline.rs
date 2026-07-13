use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use griffr_common::runtime::task_pool::{plan_archive_groups, ProgressEvent, Task, TaskPoolRunner};

use super::*;
use crate::progress::{ArchivePipelineProgress, StepProgress, VerifyTaskProgressTracker};
use crate::ui;
use crate::GlobalOptions;

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
        let mut verify_progress =
            VerifyTaskProgressTracker::new(verify_bar.clone(), verify_task_count);
        let verify_result = task_pool_runner.run_batch_with_progress(
            verify_tasks,
            Some(&mut |event: &ProgressEvent| verify_progress.handle_event(event)),
        )?;
        verify_bar.finish();
        let verify_failures = verify_result
            .events
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !verify_failures.is_empty() {
            anyhow::bail!(
                "Predownload apply requires complete staged archives; missing/mismatched items: {}",
                verify_failures.join(", ")
            );
        }

        let mut progress =
            ArchivePipelineProgress::new(&format!("update.{label}.apply"), 0, opts.verbose);
        let result = task_pool_runner.run_batch_with_progress(
            extract_tasks,
            Some(&mut |event: &ProgressEvent| progress.handle_event(event)),
        )?;
        progress.finish();

        let failures = result
            .events
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
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

    let mut progress =
        ArchivePipelineProgress::new(&format!("update.{label}"), archives.len(), opts.verbose);
    let result = task_pool_runner.run_batch_with_progress(
        tasks,
        Some(&mut |event: &ProgressEvent| progress.handle_event(event)),
    )?;
    progress.finish();

    let mut failures = Vec::new();
    for event in result.events {
        if let ProgressEvent::Failed { path, reason } = event {
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
