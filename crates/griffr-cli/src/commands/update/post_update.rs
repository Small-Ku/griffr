use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::runtime::task_pool::{Task, TaskOutcome, TaskPoolRunner, TaskProgress};
use griffr_common::runtime::{
    finish_vfs_plan, is_launcher_metadata_path, run_integrity_pool, sync_launcher_metadata,
    ContentPlan, IntegritySelection, ProgressLane, VfsTaskPlan,
};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostUpdateResult {
    Verified,
    VerificationDeferred,
}

pub(super) async fn verify_updated_install(
    api_client: &ApiClient,
    api_target: &griffr_common::config::ApiTarget,
    content_plan: &mut ContentPlan,
    skip_verify: bool,
    mut vfs_plan: VfsTaskPlan,
    selection: IntegritySelection,
    verified_artifacts: Vec<griffr_common::runtime::ArtifactProof>,
    reuse_roots: &[PathBuf],
    allow_copy_fallback: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<PostUpdateResult> {
    content_plan
        .refresh_delivery(api_client, api_target)
        .await
        .context("Failed to refresh update delivery URLs")?;
    let install_path = content_plan.install_root().to_path_buf();
    if skip_verify {
        run_extra_tasks_without_integrity(
            std::mem::take(&mut vfs_plan.tasks),
            opts,
            task_pool_runner,
        )?;
        finish_vfs_plan(&install_path, &vfs_plan, true)
            .await
            .context("Failed to finish the resource baseline after update tasks")?;
        ui::print_info(
            "Skipping post-update integrity verification (--skip-verify); launcher metadata remains unchanged until verify --repair closes the saved change",
        );
        return Ok(PostUpdateResult::VerificationDeferred);
    }

    match &selection {
        IntegritySelection::Full => {
            ui::print_info("Post-update integrity scope: full manifest plus planned VFS tasks");
        }
        IntegritySelection::GameFiles => {
            ui::print_info("Post-update integrity scope: all game files plus planned VFS tasks");
        }
        IntegritySelection::Core => {
            ui::print_info("Post-update integrity scope: core game files");
        }
        IntegritySelection::Resources => {
            ui::print_info("Post-update integrity scope: launcher resource baseline");
        }
        IntegritySelection::Paths(paths) => {
            ui::print_info(format!(
                "Post-update integrity scope: {} changed archive path(s) plus planned VFS tasks",
                paths.len()
            ));
        }
    }
    let verify_progress =
        CountAndByteProgress::new("update.verify", "update.repair.download", opts.verbose);
    let verify_session = verify_progress.start(
        ProgressLane::INTEGRITY_VERIFY,
        ProgressLane::INTEGRITY_DOWNLOAD,
    );
    let summary = run_integrity_pool(
        content_plan,
        selection,
        &verified_artifacts,
        true,
        reuse_roots,
        allow_copy_fallback,
        false,
        std::mem::take(&mut vfs_plan.tasks),
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

    finish_vfs_plan(&install_path, &vfs_plan, true)
        .await
        .context("Failed to finish the resource baseline after update verification")?;
    content_plan
        .refresh_delivery(api_client, api_target)
        .await
        .context("Failed to refresh launcher metadata URLs")?;
    sync_launcher_metadata(api_client, &install_path, content_plan.snapshot())
        .await
        .context("Failed to sync launcher metadata after update")?;
    Ok(PostUpdateResult::Verified)
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
    let result = task_pool_runner
        .run_batch(extra_tasks, task_progress)
        .context("Failed to run extra DAG tasks during skip-verify")?;
    progress_session.finish();
    progress.finish();

    let mut failures = result
        .outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            TaskOutcome::Failed { path, reason } => Some(format!("{path}: {reason}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    let failed_graph_nodes = result
        .metrics
        .graph
        .failed_nodes
        .saturating_add(result.metrics.graph.cancelled_nodes);
    if failed_graph_nodes > failures.len() {
        failures.push(format!(
            "{} additional failed or cancelled graph node(s)",
            failed_graph_nodes - failures.len()
        ));
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Extra update tasks failed while post-update verification was skipped: {}",
            failures.join("; ")
        );
    }
    Ok(())
}
