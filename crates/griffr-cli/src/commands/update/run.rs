use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::runtime::task_pool::{archive_expected_files, TaskPoolRunner};
use griffr_common::runtime::{
    plan_vfs_tasks, resolve_staged_patch_recovery_dir, select_update_package,
    streaming_assets_path, UpdatePackageKind, VfsFilePlanOptions,
};

use super::*;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

pub(super) async fn update_internal(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    use_predownload: bool,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    predownload_dir_override: Option<PathBuf>,
    require_staged_predownload: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let mut local = detect_local_install(&path).await?;
    let mut resumed_pending_transaction = false;
    match griffr_common::runtime::get_patch_recovery_state(&local.install_path, None)? {
        griffr_common::runtime::PatchRecoveryState::ExtractedReady
        | griffr_common::runtime::PatchRecoveryState::DeletePending => {
            if opts.is_dry_run() {
                opts.dry_run(format!(
                    "Would resume pending patch transaction under {} before checking for another update",
                    local.install_path.display()
                ));
                return Ok(());
            }
            crate::commands::predownload::resume(local.install_path.clone(), opts).await?;
            local = detect_local_install(&path).await?;
            resumed_pending_transaction = true;
        }
        griffr_common::runtime::PatchRecoveryState::ExtractedIncomplete { missing } => {
            if !require_staged_predownload {
                anyhow::bail!(
                    "Pending patch transaction under {} is incomplete: {}. Replay its staged archives with `predownload apply --output-dir`.",
                    local.install_path.display(),
                    missing.join(", ")
                );
            }
            ui::print_info(format!(
                "Pending extracted patch state is incomplete; replaying staged archives: {}",
                missing.join(", ")
            ));
        }
        griffr_common::runtime::PatchRecoveryState::Inconsistent { reasons } => {
            if !require_staged_predownload {
                anyhow::bail!(
                    "Pending patch state under {} is inconsistent: {}",
                    local.install_path.display(),
                    reasons.join("; ")
                );
            }
            ui::print_info(format!(
                "Pending patch state is inconsistent; replaying staged archives: {}",
                reasons.join("; ")
            ));
        }
        griffr_common::runtime::PatchRecoveryState::ArchiveReady { .. }
        | griffr_common::runtime::PatchRecoveryState::Complete => {}
    }
    if resumed_pending_transaction && require_staged_predownload {
        ui::print_success("Pending staged predownload transaction completed");
        return Ok(());
    }

    let game_id = local.require_known_game()?;
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let current_version = local.require_config_ini_version()?.to_string();
    let mut package_request_version = current_version.clone();
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let api_client = ApiClient::new()?;
    let task_pool_cfg = opts.task_pool_config();
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;

    let mut version_info = api_client
        .get_latest_game(&install_target.api, Some(&current_version))
        .await?;
    let mut recovery_stage_dir = None;

    if require_staged_predownload
        && (current_version == version_info.version || !version_info.has_update())
    {
        let (stage_dir, request_version) = resolve_staged_patch_recovery_dir(
            &local.install_path,
            predownload_dir_override.as_deref(),
            &current_version,
        )?;
        let recovery_version_info = api_client
            .get_latest_game(&install_target.api, Some(&request_version))
            .await?;
        if recovery_version_info.version != current_version || !recovery_version_info.has_update() {
            anyhow::bail!(
                "Staged predownload recovery {} resolves {} to target {}, not installed target {}.",
                stage_dir.display(),
                request_version,
                recovery_version_info.version,
                current_version
            );
        }
        ui::print_info(format!(
            "Recovering staged predownload transition {} -> {} from {}",
            request_version,
            current_version,
            stage_dir.display()
        ));
        package_request_version = request_version;
        version_info = recovery_version_info;
        recovery_stage_dir = Some(stage_dir);
    }

    ui::print_phase(format!(
        "Updating {} (region={}, channel={}, sub-channel={}) at {}",
        game_id,
        region_id,
        channel_id.channel(),
        channel_id.sub_channel(),
        local.install_path.display(),
    ));
    ui::print_info(format!(
        "Current version (config.ini): {} | Latest version: {}",
        current_version, version_info.version
    ));
    if opts.verbose {
        ui::print_info(format!(
            "Update API versions: request_version='{}' response.request_version='{}' target_version='{}'",
            package_request_version, version_info.request_version, version_info.version
        ));
    }

    if current_version == version_info.version && recovery_stage_dir.is_none()
        || !version_info.has_update()
    {
        if require_staged_predownload {
            anyhow::bail!(
                "Predownload apply requires the live release patch to be available; current version {} is still reported as up to date.",
                current_version
            );
        }
        ui::print_success("Already up to date");
        return Ok(());
    }

    let package_kind = if opts.force_full_package {
        UpdatePackageKind::Full
    } else {
        select_update_package(&version_info, Some(&package_request_version))?
    };
    let predownload_stage_dir = if use_predownload && package_kind == UpdatePackageKind::Patch {
        Some(
            recovery_stage_dir
                .or(predownload_dir_override)
                .unwrap_or_else(|| {
                    crate::commands::predownload::stage_dir_for_request(
                        &local.install_path,
                        &version_info,
                        &package_request_version,
                        &version_info.version,
                    )
                }),
        )
    } else {
        None
    };

    ui::print_info(describe_update_package_selection(
        &version_info,
        Some(&package_request_version),
        package_kind,
        opts.force_full_package,
    ));
    if require_staged_predownload && package_kind != UpdatePackageKind::Patch {
        anyhow::bail!(
            "Predownload apply requires a live patch update for the installed version; got {:?}",
            package_kind
        );
    }
    if let Some(stage_dir) = predownload_stage_dir.as_ref() {
        if use_predownload {
            ui::print_info(format!(
                "Predownload stage dir: {}{}",
                stage_dir.display(),
                if require_staged_predownload {
                    " (apply-only mode)"
                } else {
                    ""
                }
            ));
        }
    }

    if opts.is_dry_run() {
        for line in build_update_dry_run_plan(
            &local.install_path,
            &package_request_version,
            &version_info,
            package_kind,
            &reuse_paths,
            use_predownload,
            predownload_stage_dir.as_deref(),
            opts.skip_verify,
            opts.skip_vfs,
            opts.keep_pack_archives,
            opts.force_full_package,
        ) {
            opts.dry_run(line);
        }
        if require_staged_predownload {
            opts.dry_run("Would fail instead of downloading if staged predownload archives are missing or mismatched.");
        }
        return Ok(());
    }

    let expected_archive_files = if reuse_paths.is_empty() {
        if let Some(pkg) = version_info.pkg.as_ref() {
            archive_expected_files(
                api_client
                    .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
                    .await
                    .context("Failed to fetch target game_files before archive streaming")?,
            )
        } else {
            archive_expected_files(Vec::new())
        }
    } else {
        archive_expected_files(Vec::new())
    };

    let mut modified_paths = Vec::new();
    if !reuse_paths.is_empty() {
        ui::print_phase("Applying update via local file reuse");
        update_via_reuse(
            &api_client,
            &local,
            &version_info,
            &reuse_paths,
            force_copy,
            &opts,
            &mut task_pool_runner,
        )
        .await?;
    }

    if reuse_paths.is_empty() {
        match package_kind {
            UpdatePackageKind::Patch => {
                validate_patch_target(&install_target.executable, &local.install_path).await?;
                let patch = version_info
                    .patch
                    .as_ref()
                    .context("No patch package information available")?;
                let patch_password = patch.cd_key.as_deref();
                if let Some(stage_dir) = predownload_stage_dir.as_ref() {
                    modified_paths = download_and_extract_archives_from_dir(
                        &patch.patches,
                        stage_dir,
                        &local.install_path,
                        "patch",
                        opts.keep_pack_archives,
                        patch_password,
                        if require_staged_predownload {
                            ArchiveAcquireMode::RequireExisting
                        } else {
                            ArchiveAcquireMode::DownloadIfMissing
                        },
                        &patch_options,
                        expected_archive_files.clone(),
                        &opts,
                        &mut task_pool_runner,
                    )
                    .await?;
                } else {
                    modified_paths = download_and_extract_archives(
                        &patch.patches,
                        &local.install_path,
                        "patch",
                        opts.keep_pack_archives,
                        patch_password,
                        &patch_options,
                        expected_archive_files.clone(),
                        &opts,
                        &mut task_pool_runner,
                    )
                    .await?;
                }
            }
            UpdatePackageKind::Full => {
                let pkg = version_info
                    .pkg
                    .as_ref()
                    .context("No full package information available")?;
                modified_paths = download_and_extract_archives(
                    &pkg.packs,
                    &local.install_path,
                    "full",
                    opts.keep_pack_archives,
                    None,
                    &patch_options,
                    expected_archive_files.clone(),
                    &opts,
                    &mut task_pool_runner,
                )
                .await?;
            }
        }
    }

    let extra_tasks = if !opts.skip_vfs {
        ui::print_phase("Verifying update + syncing VFS resources (single DAG batch)");
        ui::print_info(
            "VFS scope: StreamingAssets index-full (Persistent VFS setup is a separate command).",
        );
        let streaming_assets =
            streaming_assets_path(&local.install_path.join(install_target.data_root.clone()));
        let source_streaming_assets = reuse_paths
            .iter()
            .filter(|path| **path != local.install_path)
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
            griffr_common::runtime::VfsPlanOutcome::Planned(plan) => plan.tasks,
            griffr_common::runtime::VfsPlanOutcome::Unsupported => Vec::new(),
        }
    } else {
        Vec::new()
    };
    verify_updated_install(
        &api_client,
        &local.install_path,
        &install_target,
        &version_info.version,
        opts.skip_verify,
        extra_tasks,
        modified_paths,
        &opts,
        &mut task_pool_runner,
    )
    .await?;

    ui::print_success("Update complete");
    Ok(())
}

pub async fn update(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    use_predownload: bool,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    update_internal(
        path,
        overrides,
        reuse_paths,
        force_copy,
        use_predownload,
        patch_options,
        None,
        false,
        opts,
    )
    .await
}

pub(crate) async fn apply_staged_predownload(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    predownload_dir_override: Option<PathBuf>,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    update_internal(
        path,
        overrides,
        Vec::new(),
        false,
        true,
        patch_options,
        predownload_dir_override,
        true,
        opts,
    )
    .await
}
