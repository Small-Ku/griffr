use std::path::Path;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::InstallTarget;
use griffr_common::runtime::task_pool::{Task, TaskPoolRunner, TaskProgress};
use griffr_common::runtime::{
    is_launcher_metadata_path, run_integrity_pool, sync_launcher_metadata, IntegritySelection,
    ProgressLane,
};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;

pub(super) async fn verify_updated_install(
    api_client: &ApiClient,
    install_path: &Path,
    install_target: &InstallTarget,
    target_version: &str,
    skip_verify: bool,
    extra_tasks: Vec<Task>,
    modified_paths: Vec<String>,
    already_verified_paths: Vec<String>,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    if skip_verify {
        run_extra_tasks_without_integrity(extra_tasks, opts, task_pool_runner)?;
        ui::print_info("Skipping post-update integrity verification (--skip-verify)");
        sync_launcher_metadata(
            api_client,
            install_path,
            install_target,
            Some(target_version),
        )
        .await
        .context("Failed to sync launcher metadata after update")?;
        return Ok(());
    }

    let modified_path_count = modified_paths.len();
    ui::print_info(format!(
        "Post-update integrity scope: {} modified archive path(s) plus planned VFS tasks",
        modified_path_count
    ));
    let verify_progress =
        CountAndByteProgress::new("update.verify", "update.repair.download", opts.verbose);
    let verify_session = verify_progress.start(
        ProgressLane::INTEGRITY_VERIFY,
        ProgressLane::INTEGRITY_DOWNLOAD,
    );
    let summary = run_integrity_pool(
        api_client,
        install_path,
        install_target,
        Some(target_version),
        IntegritySelection::Paths(modified_paths),
        &already_verified_paths,
        true,
        &[],
        false,
        false,
        extra_tasks,
        Some(task_pool_runner),
        verify_session.sender(),
    )
    .await?;
    verify_session.finish();
    verify_progress.finish();

    ui::print_info(format!(
        "Verification summary: verified={} issues={} repaired_downloads={}",
        summary.verified_files,
        summary.issues.len(),
        summary.downloaded_files
    ));
    for issue in summary.issues.iter().take(20) {
        ui::print_warning(format!("{} {:?}", issue.path, issue.kind));
    }
    let remaining_non_metadata = summary
        .issues
        .iter()
        .filter(|issue| !is_launcher_metadata_path(&issue.path))
        .count();
    if remaining_non_metadata > 0 {
        anyhow::bail!(
            "Post-update integrity has {} non-metadata issue(s). Re-run `griffr verify --path \"{}\" --repair` and then `griffr update --path \"{}\"`.",
            remaining_non_metadata,
            install_path.display(),
            install_path.display()
        );
    }

    sync_launcher_metadata(
        api_client,
        install_path,
        install_target,
        Some(target_version),
    )
    .await
    .context("Failed to sync launcher metadata after update")?;
    Ok(())
}

fn run_extra_tasks_without_integrity(
    extra_tasks: Vec<Task>,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    if extra_tasks.is_empty() {
        return Ok(());
    }

    let progress = CountAndByteProgress::new(
        "update.vfs-sync.verify",
        "update.vfs-sync.download",
        opts.verbose,
    );
    let verify_lane = ProgressLane::VFS_VERIFY;
    let download_lane = ProgressLane::VFS_DOWNLOAD;
    let progress_session = progress.start(verify_lane, download_lane);
    let task_progress = TaskProgress::new(progress_session.sender())
        .with_verify(verify_lane, extra_tasks.len())
        .with_download(download_lane);
    let _ = task_pool_runner
        .run_batch(extra_tasks, task_progress)
        .context("Failed to run extra DAG tasks during skip-verify")?;
    progress_session.finish();
    progress.finish();
    Ok(())
}
