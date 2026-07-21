use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::PackageInfo;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::{
    archive_expected_files, plan_archive_groups, ArchiveRetention, ArchiveSource, Task,
    TaskGraphBuilder, TaskOutcome, TaskPoolRunner, TaskProgress,
};
use griffr_common::runtime::{
    directory_has_entries, ensure_game_files_with_pool, is_launcher_metadata_path, plan_vfs_tasks,
    resolve_file_reuse_sources, run_integrity_pool, streaming_assets_path, sync_launcher_metadata,
    FileReuseConfig, ProgressLane, VfsFilePlanOptions,
};

use crate::commands::archive_graph::{add_file_tasks, owned_archive_paths};
use crate::progress::{ArchiveProgress, CountAndByteProgress};
use crate::ui;
use crate::GlobalOptions;

pub(super) fn validate_install_disk_space(
    package: &PackageInfo,
    install_path: &Path,
) -> Result<()> {
    let required_bytes = griffr_common::runtime::required_install_bytes(package);
    let Some(available_bytes) = griffr_common::runtime::available_space(install_path)? else {
        return Ok(());
    };

    if available_bytes < required_bytes {
        anyhow::bail!(
            "Insufficient disk space for install at {}: required {} ({}), available {} ({}), shortfall {} ({})",
            install_path.display(),
            required_bytes,
            ui::format_bytes(required_bytes),
            available_bytes,
            ui::format_bytes(available_bytes),
            required_bytes - available_bytes,
            ui::format_bytes(required_bytes - available_bytes)
        );
    }

    Ok(())
}

pub async fn install(
    game_id: GameId,
    region_id: RegionId,
    channel_id: ChannelPair,
    overrides: crate::InstallTargetOverrideArgs,
    install_path: PathBuf,
    force: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let install_path_exists = match compio::fs::metadata(&install_path).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to stat install path {}", install_path.display()))
        }
    };

    if install_path_exists && !force && directory_has_entries(install_path.clone()).await? {
        anyhow::bail!(
            "Install path is not empty: {} (pass --force to reuse it)",
            install_path.display()
        );
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would install {:?} region={} {:?} into {}",
            game_id,
            region_id,
            channel_id,
            install_path.display()
        ));
        if opts.keep_pack_archives {
            opts.dry_run(
                "Would stream archive ranges during extraction, retain them, fill only missing gaps, verify each full volume, and keep the package archives.",
            );
        } else {
            opts.dry_run("Would stream required package byte ranges, verify extracted files, and remove the range cache after commit.");
        }
        if !reuse_paths.is_empty() {
            opts.dry_run(format!(
                "Would reuse files from: {}",
                reuse_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Ok(());
    }

    compio::fs::create_dir_all(&install_path)
        .await
        .with_context(|| format!("Failed to create {}", install_path.display()))?;

    let api_client = ApiClient::new()?;
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let version_info = api_client
        .get_latest_game(&install_target.api, None)
        .await
        .context("Failed to fetch version information")?;

    let pkg = version_info
        .pkg
        .as_ref()
        .context("No package information available")?;
    let total_size: u64 = pkg.packs.iter().map(|p| p.size()).sum();
    validate_install_disk_space(pkg, &install_path)?;

    ui::print_phase(format!(
        "Installing {} (region={}, channel={}, sub-channel={}) into {}",
        game_id,
        region_id,
        channel_id.channel(),
        channel_id.sub_channel(),
        install_path.display()
    ));
    ui::print_info(format!(
        "Target version: {} | Archives: {} | Size: {}",
        version_info.version,
        pkg.packs.len(),
        ui::format_bytes(total_size)
    ));
    if !reuse_paths.is_empty() {
        ui::print_info(format!("Reuse sources: {}", reuse_paths.len()));
    }

    let task_pool_cfg = opts.task_pool_config();
    let mut task_pool = TaskPoolRunner::new(task_pool_cfg)?;

    let mut extra_tasks = if !opts.skip_vfs {
        ui::print_phase("Planning VFS resources for the install DAG");
        ui::print_info(
            "VFS scope: StreamingAssets index-full (Persistent VFS setup is a separate command).",
        );
        let streaming_assets =
            streaming_assets_path(&install_path.join(install_target.data_root.clone()));
        let source_streaming_assets = reuse_paths
            .iter()
            .filter(|path| **path != install_path)
            .map(|path| streaming_assets_path(&path.join(install_target.data_root.clone())))
            .collect::<Vec<_>>();
        let rand_str = version_info.rand_str();
        match plan_vfs_tasks(
            &api_client,
            &install_target.api,
            &version_info.version,
            &rand_str,
            &streaming_assets,
            &VfsFilePlanOptions {
                source_streaming_assets,
                allow_repair: true,
                allow_copy_fallback: force_copy,
                prefer_reuse: false,
            },
        )
        .await
        .context("Failed to plan VFS tasks")?
        {
            Some(plan) => plan.tasks,
            None => {
                ui::print_info("The selected target does not provide the launcher resource-index API. Skip VFS sync.");
                Vec::new()
            }
        }
    } else {
        ui::print_phase("Verifying install integrity");
        Vec::new()
    };

    let mut already_verified_paths = Vec::new();
    if reuse_paths.is_empty() {
        ui::print_phase("Downloading and extracting archives");
        let download_dir = install_path.join("downloads");
        compio::fs::create_dir_all(&download_dir)
            .await
            .with_context(|| format!("Failed to create {}", download_dir.display()))?;

        let archive_groups = plan_archive_groups(&pkg.packs, &download_dir)?;
        let expected_archive_files = archive_expected_files(
            api_client
                .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
                .await
                .context("Failed to fetch game_files before archive streaming")?,
        );
        let archive_group_count = archive_groups.len();
        let excluded_commit_paths =
            owned_archive_paths(&extra_tasks, &install_path, expected_archive_files.as_ref());
        let archive_verify_count = if opts.keep_pack_archives {
            pkg.packs.len()
        } else {
            archive_group_count
        };
        let mut graph = TaskGraphBuilder::new();
        let mut archive_nodes = Vec::with_capacity(archive_groups.len());
        for group in archive_groups {
            archive_nodes.push(graph.add_root(Task::OpenArchive {
                base_name: group.base_name,
                source: ArchiveSource::Remote(group.parts),
                dest: install_path.clone(),
                retention: ArchiveRetention::from_keep_full_volumes(opts.keep_pack_archives),
                password: None,
                patch_options: griffr_common::runtime::PatchApplyOptions::default(),
                expected_files: expected_archive_files.clone(),
                excluded_commit_paths: excluded_commit_paths.clone(),
            }));
        }
        let archive_vfs_task_count = extra_tasks.len();
        let (parallel_vfs, dependent_vfs) = add_file_tasks(
            &mut graph,
            std::mem::take(&mut extra_tasks),
            &archive_nodes,
            &install_path,
            expected_archive_files.as_ref(),
            false,
        )?;
        opts.verbose(format!(
            "VFS/archive ownership: {parallel_vfs} independent task(s), {dependent_vfs} archive-dependent task(s)"
        ));

        let progress = ArchiveProgress::new("install", opts.verbose);
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
                archive_verify_count
                    .saturating_add(
                        expected_archive_files
                            .len()
                            .saturating_sub(excluded_commit_paths.len()),
                    )
                    .saturating_add(archive_vfs_task_count),
            )
            .with_download(download_lane)
            .with_extract(extract_lane)
            .with_commit(commit_lane)
            .with_patch(patch_lane)
            .with_delete(delete_lane);
        let result = task_pool.run_graph(graph.build_checked()?, task_progress)?;
        progress_session.finish();
        progress.finish();

        for outcome in &result.outcomes {
            if let TaskOutcome::ArchiveCheck { report, .. } = outcome {
                ui::print_patch_check(report);
            }
        }

        let mut failures = Vec::new();
        for event in result.outcomes {
            match event {
                TaskOutcome::Verified { path, ok: true, .. }
                    if expected_archive_files
                        .contains_key(&path.replace('\\', "/").to_ascii_lowercase()) =>
                {
                    already_verified_paths.push(path);
                }
                TaskOutcome::Failed { path, reason } => {
                    failures.push(format!("{} ({})", path, reason));
                }
                _ => {}
            }
        }
        if !failures.is_empty() {
            anyhow::bail!(
                "Install archive work failed for {} item(s): {}",
                failures.len(),
                failures.join(", ")
            );
        }
    } else {
        ui::print_phase("Ensuring files from reuse sources");
        let source_installs =
            resolve_file_reuse_sources(&game_id, &install_path, &reuse_paths).await?;
        let ensure_progress = CountAndByteProgress::new(
            "install.ensure_files",
            "install.ensure_files.download",
            opts.verbose,
        );
        let ensure_session = ensure_progress.start(
            ProgressLane::FILE_ENSURE_VERIFY,
            ProgressLane::FILE_ENSURE_DOWNLOAD,
        );
        let ensured = ensure_game_files_with_pool(
            &api_client,
            game_id,
            &install_path,
            &pkg.file_path,
            pkg.game_files_md5.as_deref(),
            &FileReuseConfig {
                allow_copy_fallback: force_copy,
                dry_run: false,
                source_installs,
            },
            Some(&mut task_pool),
            ensure_session.sender(),
        )
        .await
        .context("Failed to ensure files during install")?;
        ensure_session.finish();
        ensure_progress.finish();
        ui::print_info(format!(
            "Ensured files: reused={} downloaded={}",
            ensured.reused_files, ensured.downloaded_files
        ));
        if !ensured.issues.is_empty() {
            anyhow::bail!(
                "Install file ensure work finished with {} issue(s)",
                ensured.issues.len()
            );
        }
    }

    sync_launcher_metadata(
        &api_client,
        &install_path,
        &install_target,
        Some(&version_info.version),
    )
    .await
    .context("Failed to sync launcher metadata after install staging")?;

    let verify_progress =
        CountAndByteProgress::new("install.verify", "install.repair.download", opts.verbose);
    let verify_session = verify_progress.start(
        ProgressLane::INTEGRITY_VERIFY,
        ProgressLane::INTEGRITY_DOWNLOAD,
    );
    let summary = run_integrity_pool(
        &api_client,
        &install_path,
        &install_target,
        Some(&version_info.version),
        griffr_common::runtime::IntegritySelection::Full,
        &already_verified_paths,
        true,
        &[],
        false,
        false,
        extra_tasks,
        Some(&mut task_pool),
        verify_session.sender(),
    )
    .await?;
    verify_session.finish();
    verify_progress.finish();
    if !summary.issues.is_empty() {
        let mut repairable_issues = Vec::new();
        let mut metadata_issues = Vec::new();
        for issue in summary.issues.iter().take(20) {
            ui::print_warning(format!(
                "integrity issue path={} kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
                issue.path,
                issue.kind,
                issue.expected_size,
                issue.actual_size,
                issue.expected_md5,
                issue.actual_md5
            ));
        }
        for issue in &summary.issues {
            if is_launcher_metadata_path(&issue.path) {
                metadata_issues.push(issue.clone());
            } else {
                repairable_issues.push(issue.clone());
            }
        }

        if !metadata_issues.is_empty() {
            ui::print_info(format!(
                "Ignored metadata-only issues: {} (launcher metadata files will be normalized)",
                metadata_issues.len()
            ));
        }

        if !repairable_issues.is_empty() {
            anyhow::bail!(
                "Post-install integrity still reports {} non-metadata issue(s)",
                repairable_issues.len()
            );
        }
    }

    ui::print_success("Install finished");
    Ok(())
}
