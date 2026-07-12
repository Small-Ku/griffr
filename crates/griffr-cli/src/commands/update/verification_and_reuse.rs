use rapidhash::RapidHashMap as HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::InstallProfile;
use griffr_common::runtime::task_pool::{ArchivePart, ProgressEvent, Task, TaskPoolRunner};
use griffr_common::runtime::{
    is_launcher_metadata_path, materialize_game_files_with_pool, run_integrity_pool,
    sync_launcher_metadata, FileReuseConfig, SourceInstallInput,
};

use crate::commands::local::detect_local_install;
use crate::progress::{ByteProgressTracker, StepProgress, VerifyTaskProgressTracker};
use crate::ui;
use crate::GlobalOptions;

pub(super) async fn verify_updated_install(
    api_client: &ApiClient,
    install_path: &Path,
    profile: &InstallProfile,
    target_version: &str,
    skip_verify: bool,
    extra_tasks: Vec<Task>,
    extra_task_total_bytes: u64,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    if skip_verify {
        if !extra_tasks.is_empty() {
            let bar = StepProgress::new("update.vfs-sync", opts.verbose);
            let mut byte_progress = ByteProgressTracker::new(bar.clone(), extra_task_total_bytes);
            let mut verify_progress =
                VerifyTaskProgressTracker::new(bar.clone(), extra_tasks.len());
            let _ = task_pool_runner
                .run_batch_with_progress(
                    extra_tasks,
                    Some(&mut |event: &ProgressEvent| {
                        byte_progress.handle_event(event);
                        verify_progress.handle_event(event);
                    }),
                )
                .context("Failed to execute extra DAG tasks during skip-verify")?;
            bar.finish();
        }
        ui::print_info("Skipping post-update integrity verification (--skip-verify)");
        sync_launcher_metadata(api_client, install_path, profile, Some(target_version))
            .await
            .context("Failed to sync launcher metadata after update")?;
        return Ok(());
    }

    let verify_bar = StepProgress::new("update.verify+repair", opts.verbose);
    let (cb1, cb2) = verify_bar.split_callbacks();
    let summary = run_integrity_pool(
        api_client,
        install_path,
        profile,
        Some(target_version),
        true,
        &[],
        false,
        false,
        extra_tasks,
        Some(task_pool_runner),
        Some(cb1),
        Some(cb2),
    )
    .await?;
    verify_bar.finish();
    ui::print_info(format!(
        "Verification summary: issues={} repaired_downloads={}",
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

    sync_launcher_metadata(api_client, install_path, profile, Some(target_version))
        .await
        .context("Failed to sync launcher metadata after update")?;
    Ok(())
}

pub(super) async fn update_via_reuse(
    api_client: &ApiClient,
    local: &crate::commands::local::LocalInstall,
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    reuse_paths: &[PathBuf],
    force_copy: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    let game_id = local.require_known_game()?;
    let target_channel_id = local.require_known_channel()?;

    let pkg = version_info
        .pkg
        .as_ref()
        .context("No full package information available for reuse update")?;

    let mut source_installs = Vec::new();

    for reuse_path in reuse_paths {
        let source = detect_local_install(reuse_path)
            .await
            .with_context(|| format!("Failed to inspect reuse source {}", reuse_path.display()))?;
        let source_game_id = source.require_known_game()?;
        if source_game_id != game_id {
            anyhow::bail!(
                "Reuse source {} is {:?}, expected {:?}",
                source.install_path.display(),
                source_game_id,
                game_id
            );
        }
        let source_channel_id = source.require_known_channel()?;
        let source_version = source.require_config_ini_version()?.to_string();
        if source.install_path == local.install_path {
            continue;
        }
        source_installs.push(SourceInstallInput {
            channel_id: source_channel_id,
            version: source_version,
            install_path: source.install_path.clone(),
        });
    }

    opts.verbose(format!(
        "Applying file reuse from {} source install(s)",
        reuse_paths.len()
    ));

    let materialize_bar = StepProgress::new("update.materialize", opts.verbose);
    let (materialize_progress_cb, materialize_download_cb) = materialize_bar.split_callbacks();
    let materialized = materialize_game_files_with_pool(
        api_client,
        game_id,
        target_channel_id,
        &version_info.version,
        &local.install_path,
        &pkg.file_path,
        pkg.game_files_md5.as_deref(),
        &FileReuseConfig {
            allow_copy_fallback: force_copy,
            dry_run: opts.is_dry_run(),
            source_installs,
        },
        Some(task_pool_runner),
        Some(materialize_progress_cb),
        Some(materialize_download_cb),
    )
    .await?;
    materialize_bar.finish();

    ui::print_info(format!(
        "Materialized files: reused={} downloaded={}",
        materialized.reused_files, materialized.downloaded_files
    ));
    if !materialized.issues.is_empty() {
        anyhow::bail!(
            "Update materialization finished with {} issue(s)",
            materialized.issues.len()
        );
    }
    Ok(())
}

pub(super) fn group_archives_by_base(
    archives: &[griffr_common::api::types::PackFile],
    archive_dir: &Path,
) -> Result<HashMap<String, Vec<ArchivePart>>> {
    let mut grouped: HashMap<String, Vec<ArchivePart>> = HashMap::default();
    for archive in archives {
        let filename = archive
            .filename()
            .context("Failed to extract archive filename")?
            .to_string();
        let base = archive
            .archive_base_name()
            .context("Could not determine archive base name from pack URL")?
            .to_string();
        grouped.entry(base).or_default().push(ArchivePart {
            url: archive.url.clone(),
            dest: archive_dir.join(&filename),
            logical_path: filename,
            expected_md5: archive.md5.clone(),
            expected_size: archive.size(),
        });
    }
    if grouped.is_empty() {
        anyhow::bail!("No archives to process");
    }
    Ok(grouped)
}
