use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::types::{GameFileEntry, GetLatestGameResponse};
use griffr_common::runtime::task_pool::TaskPoolRunner;
use griffr_common::runtime::{
    ensure_game_files_from_manifest_with_pool, is_launcher_metadata_path,
    is_resource_baseline_path, remove_blocking_obsolete_game_files, remove_obsolete_game_files,
    ContentPlan, FileReuseConfig, LocalInstall, ProgressLane,
};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;

pub(super) async fn update_via_reuse(
    local: &LocalInstall,
    version_info: &GetLatestGameResponse,
    content_plan: &ContentPlan,
    reuse_paths: &[PathBuf],
    current_manifest: &[GameFileEntry],
    force_copy: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    let pkg = version_info
        .pkg
        .as_ref()
        .context("No full package information available for reuse update")?;
    let target_manifest = content_plan.core_game_entries();
    let current_manifest = current_manifest
        .iter()
        .filter(|entry| {
            !is_launcher_metadata_path(&entry.path) && !is_resource_baseline_path(&entry.path)
        })
        .cloned()
        .collect::<Vec<_>>();
    opts.verbose(format!(
        "Applying file reuse from {} compatible source install(s)",
        reuse_paths.len()
    ));
    let early_cleanup = remove_blocking_obsolete_game_files(
        &local.install_path,
        &current_manifest,
        &target_manifest,
        task_pool_runner,
    )
    .await
    .context("Failed to prepare file/directory transitions for reuse update")?;

    let ensure_progress = CountAndByteProgress::new(
        "update.ensure_files",
        "update.ensure_files.download",
        opts.verbose,
    );
    let ensure_session = ensure_progress.start(
        ProgressLane::FILE_ENSURE_VERIFY,
        ProgressLane::FILE_ENSURE_DOWNLOAD,
    );
    let ensured = ensure_game_files_from_manifest_with_pool(
        &local.install_path,
        &pkg.file_path,
        &target_manifest,
        &FileReuseConfig {
            allow_copy_fallback: force_copy,
            dry_run: opts.is_dry_run(),
            source_roots: reuse_paths.to_vec(),
        },
        Some(task_pool_runner),
        ensure_session.sender(),
    )
    .await?;
    ensure_session.finish();
    ensure_progress.finish();

    ui::print_info(format!(
        "Ensured files: reused={} downloaded={}",
        ensured.reused_files, ensured.downloaded_files
    ));
    if !ensured.issues.is_empty() {
        anyhow::bail!(
            "Update file ensure work finished with {} issue(s)",
            ensured.issues.len()
        );
    }

    let cleanup = remove_obsolete_game_files(
        &local.install_path,
        &current_manifest,
        &target_manifest,
        task_pool_runner,
    )
    .await
    .context("Failed to remove files absent from the target manifest")?;
    ui::print_info(format!(
        "Obsolete launcher-owned files: removed={} retained_modified={}",
        early_cleanup
            .removed_files
            .saturating_add(cleanup.removed_files),
        cleanup.retained_modified_files
    ));
    Ok(())
}
