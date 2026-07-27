use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::{
    archive_expected_files, plan_archive_groups, ArchiveRetention, ArchiveSource, Task,
    TaskGraphBuilder, TaskOutcome, TaskPoolRunner, TaskProgress,
};
use griffr_common::runtime::{
    directory_has_entries, ensure_game_files_with_pool, finish_install_change,
    griffr_archives_path, plan_vfs_tasks, read_install_change, resolve_file_reuse_roots,
    run_integrity_pool, start_install_change, streaming_assets_path, sync_launcher_metadata,
    FileReuseConfig, InstallChangeKind, InstallChangeSource, InstallChangeStart,
    InstallChangeState, ProgressLane, VfsFilePlanOptions,
};

use crate::commands::archive_graph::{add_file_tasks, full_archive_excluded_paths};
use crate::progress::{ArchiveProgress, CountAndByteProgress};
use crate::ui;
use crate::GlobalOptions;

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
    let pending_change = read_install_change(&install_path)?;
    let can_resume_install = pending_change
        .as_ref()
        .is_some_and(|state| state.kind == InstallChangeKind::Install);
    let install_path_exists = match compio::fs::metadata(&install_path).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to stat install path {}", install_path.display()))
        }
    };

    if install_path_exists
        && !force
        && !can_resume_install
        && directory_has_entries(install_path.clone()).await?
    {
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

    let change_state = InstallChangeState::new(
        InstallChangeKind::Install,
        if reuse_paths.is_empty() {
            InstallChangeSource::FullArchive
        } else {
            InstallChangeSource::Reuse
        },
        game_id.to_string(),
        region_id.to_string(),
        channel_id.channel().to_string(),
        channel_id.sub_channel().to_string(),
        None,
        version_info.version.clone(),
        pkg.game_files_md5.clone(),
        if reuse_paths.is_empty() {
            pkg.packs.iter().map(|part| part.md5.clone()).collect()
        } else {
            Vec::new()
        },
        !opts.skip_vfs,
    );
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

    let change_start = start_install_change(&install_path, &change_state)?;
    match change_start {
        InstallChangeStart::New => {}
        InstallChangeStart::Resume => {
            ui::print_info(format!(
                "Resuming unfinished install for target {}",
                change_state.target_version
            ));
        }
        InstallChangeStart::Advance => {
            ui::print_info(format!(
                "Advancing unfinished install to target {}",
                change_state.target_version
            ));
        }
    }

    // A matching marker means a previous run already selected this exact
    // release and source identity. Resume from the target manifest instead of
    // replaying every full-archive entry: correct files are verified in place,
    // missing files use reuse/archive/CDN fallbacks, and the marker remains if
    // any final path still cannot be repaired.
    if change_start == InstallChangeStart::Resume {
        ui::print_phase("Resuming install from the target manifest");
        let source_roots = resolve_file_reuse_roots(&game_id, &install_path, &reuse_paths).await?;
        let progress = CountAndByteProgress::new(
            "install.resume.verify",
            "install.resume.download",
            opts.verbose,
        );
        let session = progress.start(
            ProgressLane::INTEGRITY_VERIFY,
            ProgressLane::INTEGRITY_DOWNLOAD,
        );
        let summary = run_integrity_pool(
            &api_client,
            &install_path,
            &install_target,
            Some(&change_state.target_version),
            griffr_common::runtime::IntegritySelection::GameFiles,
            &[],
            true,
            &source_roots,
            force_copy,
            !source_roots.is_empty(),
            extra_tasks,
            Some(&mut task_pool),
            session.sender(),
        )
        .await
        .context("Failed to resume install from the target manifest")?;
        session.finish();
        progress.finish();
        if !summary.issues.is_empty() {
            anyhow::bail!(
                "Install resume kept the change marker because {} target file issue(s) remain",
                summary.issues.len()
            );
        }
        sync_launcher_metadata(
            &api_client,
            &install_path,
            &install_target,
            Some(&change_state.target_version),
        )
        .await
        .context("Failed to sync launcher metadata after resumed install verification")?;
        finish_install_change(&install_path, &change_state)
            .context("Failed to remove the install change marker")?;
        ui::print_success("Install resume finished");
        return Ok(());
    }

    let mut already_verified_paths = Vec::new();
    if reuse_paths.is_empty() {
        ui::print_phase("Downloading and extracting archives");
        let download_dir = griffr_archives_path(&install_path);
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
        let excluded_commit_paths = full_archive_excluded_paths(
            &extra_tasks,
            &install_path,
            expected_archive_files.as_ref(),
        );
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
                            .keys()
                            .filter(|path| !excluded_commit_paths.contains(*path))
                            .count(),
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
                TaskOutcome::Committed { proof }
                    if expected_archive_files.contains_key(
                        &proof.logical_path().replace('\\', "/").to_ascii_lowercase(),
                    ) =>
                {
                    already_verified_paths.push(proof);
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
        let source_roots = resolve_file_reuse_roots(&game_id, &install_path, &reuse_paths).await?;
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
            &install_path,
            &pkg.file_path,
            pkg.game_files_md5.as_deref(),
            &FileReuseConfig {
                allow_copy_fallback: force_copy,
                dry_run: false,
                source_roots,
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
        griffr_common::runtime::IntegritySelection::GameFiles,
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
        anyhow::bail!(
            "Post-install integrity still reports {} game-file issue(s)",
            summary.issues.len()
        );
    }

    sync_launcher_metadata(
        &api_client,
        &install_path,
        &install_target,
        Some(&version_info.version),
    )
    .await
    .context("Failed to sync launcher metadata after final install verification")?;
    finish_install_change(&install_path, &change_state)
        .context("Failed to remove the install change marker")?;

    ui::print_success("Install finished");
    Ok(())
}
