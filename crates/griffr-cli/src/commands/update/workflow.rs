use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::runtime::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::runtime::{plan_vfs_tasks, streaming_assets_path, VfsMaterializeConfig};

use super::*;
use crate::commands::local::detect_local_install;
use crate::ui;
use crate::GlobalOptions;

pub(super) async fn update_internal(
    path: PathBuf,
    overrides: crate::InstallProfileOverrideArgs,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    use_predownload: bool,
    predownload_dir_override: Option<PathBuf>,
    require_staged_predownload: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let channel_id = local.require_known_channel()?;
    let current_version = local.require_config_ini_version()?.to_string();
    let profile = griffr_common::config::resolve_install_profile(
        &game_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let api_client = ApiClient::new()?;
    let task_pool_cfg = TaskPoolConfig::with_progress_buffers(
        opts.extraction_progress_buffer_bytes,
        opts.download_progress_buffer_bytes,
    );
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;

    let version_info = api_client
        .get_latest_game(&profile.target, Some(&current_version))
        .await?;

    ui::print_phase(format!(
        "Updating {} (channel={}, sub-channel={}) at {}",
        game_id,
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
            current_version, version_info.request_version, version_info.version
        ));
    }

    if current_version == version_info.version || !version_info.has_update() {
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
        choose_update_package(&version_info, Some(&current_version))?
    };
    let predownload_stage_dir = if use_predownload && package_kind == UpdatePackageKind::Patch {
        Some(predownload_dir_override.unwrap_or_else(|| {
            crate::commands::predownload::stage_dir_for_request(
                &local.install_path,
                &version_info,
                &current_version,
                &version_info.version,
            )
        }))
    } else {
        None
    };

    ui::print_info(describe_update_package_selection(
        &version_info,
        Some(&current_version),
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
            &current_version,
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
                validate_patch_target(&profile.executable, &local.install_path).await?;
                let patch = version_info
                    .patch
                    .as_ref()
                    .context("No patch package information available")?;
                let patch_password = patch.cd_key.as_deref();
                if let Some(stage_dir) = predownload_stage_dir.as_ref() {
                    download_and_extract_archives_from_dir(
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
                        &opts,
                        &mut task_pool_runner,
                    )
                    .await?;
                } else {
                    download_and_extract_archives(
                        &patch.patches,
                        &local.install_path,
                        "patch",
                        opts.keep_pack_archives,
                        patch_password,
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
                download_and_extract_archives(
                    &pkg.packs,
                    &local.install_path,
                    "full",
                    opts.keep_pack_archives,
                    None,
                    &opts,
                    &mut task_pool_runner,
                )
                .await?;
            }
        }
    }

    let (extra_tasks, extra_task_total_bytes) = if !opts.skip_vfs {
        ui::print_phase("Verifying update + syncing VFS resources (single DAG batch)");
        ui::print_info(
            "VFS scope: StreamingAssets index-full (Persistent bootstrap is a separate step).",
        );
        let streaming_assets =
            streaming_assets_path(&local.install_path.join(profile.data_root.clone()));
        let source_streaming_assets = reuse_paths
            .iter()
            .filter(|path| **path != local.install_path)
            .map(|path| streaming_assets_path(&path.join(profile.data_root.clone())))
            .collect::<Vec<_>>();
        let rand_str = version_info.rand_str();
        match plan_vfs_tasks(
            &api_client,
            &profile.target,
            &version_info.version,
            &rand_str,
            &streaming_assets,
            &VfsMaterializeConfig {
                source_streaming_assets,
                allow_copy_fallback: force_copy,
                prefer_reuse: !reuse_paths.is_empty(),
            },
        )
        .await
        .context("Failed to plan VFS tasks")?
        {
            griffr_common::runtime::VfsPlanOutcome::Planned(plan) => (plan.tasks, plan.total_bytes),
            griffr_common::runtime::VfsPlanOutcome::Unsupported => (Vec::new(), 0),
        }
    } else {
        (Vec::new(), 0)
    };
    verify_updated_install(
        &api_client,
        &local.install_path,
        &profile,
        &version_info.version,
        opts.skip_verify,
        extra_tasks,
        extra_task_total_bytes,
        &opts,
        &mut task_pool_runner,
    )
    .await?;

    ui::print_success("Update complete");
    Ok(())
}

pub async fn update(
    path: PathBuf,
    overrides: crate::InstallProfileOverrideArgs,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    use_predownload: bool,
    opts: GlobalOptions,
) -> Result<()> {
    update_internal(
        path,
        overrides,
        reuse_paths,
        force_copy,
        use_predownload,
        None,
        false,
        opts,
    )
    .await
}

pub(crate) async fn apply_staged_predownload(
    path: PathBuf,
    overrides: crate::InstallProfileOverrideArgs,
    predownload_dir_override: Option<PathBuf>,
    opts: GlobalOptions,
) -> Result<()> {
    update_internal(
        path,
        overrides,
        Vec::new(),
        false,
        true,
        predownload_dir_override,
        true,
        opts,
    )
    .await
}
