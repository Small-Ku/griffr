use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::GetLatestGameResponse;
use griffr_common::config::InstallTarget;
use griffr_common::runtime::task_pool::{archive_expected_files, TaskPoolRunner};
use griffr_common::runtime::{
    estimate_manifest_delta, finish_install_change, is_launcher_metadata_path,
    is_resource_baseline_path, patch_group_beats_manifest_sources, plan_vfs_tasks,
    read_install_change, read_local_game_files, resolve_staged_patch_recovery_dir,
    select_archive_package, start_install_change, streaming_assets_path, ArchivePackageKind,
    ContentPlan, GameManifestSnapshot, InstallChangeKind, InstallChangeSource, InstallChangeStart,
    InstallChangeState, IntegritySelection, VfsFilePlanOptions, VfsTaskPlan,
};

use super::*;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

pub(super) async fn update_internal(
    api_client: &ApiClient,
    task_pool_runner: &mut TaskPoolRunner,
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    explicit_reuse_paths: Vec<PathBuf>,
    peer_reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    use_predownload: bool,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    predownload_dir_override: Option<PathBuf>,
    require_staged_predownload: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let mut local = detect_local_install(&path).await?;
    let pending_change = read_install_change(&local.install_path)?;
    let mut resumed_pending_patch = false;
    match griffr_common::runtime::get_patch_recovery_state(&local.install_path, None)? {
        griffr_common::runtime::PatchRecoveryState::ExtractedReady
        | griffr_common::runtime::PatchRecoveryState::DeletePending => {
            if pending_change.is_none() {
                ui::print_info(format!(
                    "Found orphaned patch artifacts under {} without an install change marker; clearing leftovers...",
                    local.install_path.display()
                ));
                if opts.is_dry_run() {
                    opts.dry_run(format!(
                        "Would discard orphaned patch state under {}",
                        local.install_path.display()
                    ));
                } else {
                    griffr_common::runtime::discard_incomplete_patch_apply(&local.install_path)?;
                }
            } else {
                if opts.is_dry_run() {
                    opts.dry_run(format!(
                        "Would resume pending patch apply under {} before checking for another update",
                        local.install_path.display()
                    ));
                    return Ok(());
                }
                crate::commands::predownload::resume(local.install_path.clone(), opts).await?;
                local = detect_local_install(&path).await?;
                resumed_pending_patch = true;
            }
        }
        griffr_common::runtime::PatchRecoveryState::ExtractedMissing { missing } => {
            if require_staged_predownload {
                ui::print_info(format!(
                    "Pending extracted patch state is missing required data; replaying staged archives: {}",
                    missing.join(", ")
                ));
            } else {
                ui::print_info(format!(
                    "Incomplete patch extraction will be rebuilt from the current update archives: {}",
                    missing.join(", ")
                ));
                if opts.is_dry_run() {
                    opts.dry_run(format!(
                        "Would discard private incomplete patch state under {} before rebuilding it",
                        local.install_path.display()
                    ));
                } else {
                    griffr_common::runtime::discard_incomplete_patch_apply(&local.install_path)?;
                }
            }
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
        | griffr_common::runtime::PatchRecoveryState::Idle => {}
    }
    if resumed_pending_patch && require_staged_predownload && pending_change.is_none() {
        ui::print_success("Pending staged predownload patch finished");
        return Ok(());
    }

    let game_id = local.require_known_game()?;
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let current_version = local.require_config_ini_version()?.to_string();
    if let Some(state) = pending_change.as_ref() {
        let same_install = state.matches_install(
            &game_id.to_string(),
            &region_id.to_string(),
            channel_id.channel().as_str(),
            channel_id.sub_channel().as_str(),
        );
        if !same_install {
            anyhow::bail!(
                "Pending {} change at {} belongs to {}/{}/{}/{}, not {}/{}/{}/{}",
                state.kind,
                local.install_path.display(),
                state.game,
                state.region,
                state.channel,
                state.sub_channel,
                game_id,
                region_id,
                channel_id.channel(),
                channel_id.sub_channel()
            );
        }
        if state.kind == InstallChangeKind::Repair {
            anyhow::bail!(
                "A repair is unfinished for target {}. Run `griffr verify --path \"{}\" --repair` before update.",
                state.target_version,
                local.install_path.display()
            );
        }
    }
    let mut package_request_version = current_version.clone();
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let mut version_info = api_client
        .get_latest_game(&install_target.api, Some(&current_version))
        .await?;
    if let Some(state) = pending_change
        .as_ref()
        .filter(|state| state.target_version == version_info.version)
    {
        let live_package = version_info.pkg.as_ref();
        let live_game_files_path = live_package.map(|pkg| pkg.file_path.as_str());
        let live_game_files_md5 = live_package.and_then(|pkg| pkg.game_files_md5.as_deref());
        if !state.matches_release(
            &version_info.version,
            live_game_files_path,
            live_game_files_md5,
        ) {
            anyhow::bail!(
                "Unfinished {} target {} no longer matches its saved game_files identity; refusing to continue with changed metadata under the same version",
                state.kind,
                state.target_version
            );
        }
    }
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

    let reuse_roots = merge_reuse_paths(&explicit_reuse_paths, &peer_reuse_paths);

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

    if let Some(state) = pending_change.as_ref().filter(|state| {
        state.target_version == current_version
            && matches!(
                state.kind,
                InstallChangeKind::Install | InstallChangeKind::Update
            )
            && (current_version == version_info.version || !version_info.has_update())
    }) {
        if opts.is_dry_run() {
            opts.dry_run(format!(
                "Would verify target {} and remove the unfinished {} marker",
                state.target_version, state.kind
            ));
            return Ok(());
        }
        ui::print_phase(format!(
            "Checking unfinished {} target {}",
            state.kind, state.target_version
        ));
        let vfs_plan = plan_update_vfs_tasks(
            api_client,
            &install_target,
            &version_info,
            &local.install_path,
            &reuse_roots,
            force_copy,
            !state.sync_vfs,
        )
        .await?;
        if state.sync_vfs && vfs_plan.identity != state.resource_identity {
            anyhow::bail!(
                "Unfinished {} target {} no longer matches its saved resource identity",
                state.kind,
                state.target_version
            );
        }
        let manifest_snapshot = GameManifestSnapshot::fetch(api_client, &version_info)
            .await
            .context("Failed to fetch the unfinished update manifest snapshot")?;
        let mut content_plan =
            ContentPlan::from_snapshot(&local.install_path, manifest_snapshot, &vfs_plan.claims)
                .context("Failed to build the update content plan")?;
        let post_update = verify_updated_install(
            api_client,
            &install_target.api,
            &mut content_plan,
            false,
            vfs_plan,
            IntegritySelection::GameFiles,
            Vec::new(),
            &reuse_roots,
            force_copy,
            &opts,
            task_pool_runner,
        )
        .await?;
        debug_assert_eq!(post_update, PostUpdateResult::Verified);
        finish_install_change(&local.install_path, state)
            .context("Failed to remove the install change marker")?;
        ui::print_success(format!("Unfinished {} target verified", state.kind));
        return Ok(());
    }

    if current_version == version_info.version && recovery_stage_dir.is_none()
        || !version_info.has_update()
    {
        if let Some(state) = pending_change.as_ref() {
            anyhow::bail!(
                "Unfinished {} change targets {}, but config.ini reports {} and no matching update is available. Run `griffr verify --path \"{}\" --repair`.",
                state.kind,
                state.target_version,
                current_version,
                local.install_path.display()
            );
        }
        if require_staged_predownload {
            anyhow::bail!(
                "Predownload apply requires the live release patch to be available; current version {} is still reported as up to date.",
                current_version
            );
        }
        ui::print_success("Already up to date");
        return Ok(());
    }

    let force_full_for_mixed_recovery = pending_change.as_ref().is_some_and(|state| {
        current_version != state.target_version && version_info.version != state.target_version
    });
    if force_full_for_mixed_recovery {
        ui::print_warning(format!(
            "Unfinished target {} was superseded by {} while config.ini still reports {}. Use the full package or manifest-driven reuse so progress does not depend on obsolete patch bases.",
            pending_change
                .as_ref()
                .map(|state| state.target_version.as_str())
                .unwrap_or("unknown"),
            version_info.version,
            current_version
        ));
    }
    let current_manifest = match read_local_game_files(&local.install_path).await {
        Ok(manifest) => manifest,
        Err(error) => {
            if opts.output != crate::OutputFormat::Json {
                ui::print_warning(format!(
                    "Local game_files metadata is unavailable ({error}); falling back to an archive update."
                ));
            }
            None
        }
    };
    let manifest_snapshot = GameManifestSnapshot::fetch(api_client, &version_info)
        .await
        .context("Failed to fetch the update manifest snapshot")?;

    let force_full_archive = opts.force_full_package || force_full_for_mixed_recovery;
    let manifest_eligible = current_manifest.is_some()
        && version_info.pkg.is_some()
        && !require_staged_predownload
        && !force_full_archive
        && !use_predownload;
    let manifest_delta = current_manifest.as_deref().map(|current| {
        let current_core = current
            .iter()
            .filter(|entry| {
                !is_launcher_metadata_path(&entry.path) && !is_resource_baseline_path(&entry.path)
            })
            .cloned()
            .collect::<Vec<_>>();
        let target_core = manifest_snapshot
            .entries
            .iter()
            .filter(|entry| {
                !is_launcher_metadata_path(&entry.path) && !is_resource_baseline_path(&entry.path)
            })
            .cloned()
            .collect::<Vec<_>>();
        estimate_manifest_delta(&current_core, &target_core)
    });
    let patch_group_selected = manifest_eligible
        && manifest_delta.is_some_and(|delta| {
            patch_group_beats_manifest_sources(
                &version_info,
                Some(&package_request_version),
                delta,
                !reuse_roots.is_empty(),
            )
        });

    let archive_kind = if force_full_archive {
        Some(ArchivePackageKind::Full)
    } else if require_staged_predownload || use_predownload {
        Some(select_archive_package(
            &version_info,
            Some(&package_request_version),
        )?)
    } else if patch_group_selected {
        Some(ArchivePackageKind::Patch)
    } else if manifest_eligible {
        None
    } else {
        Some(select_archive_package(
            &version_info,
            Some(&package_request_version),
        )?)
    };
    let use_manifest_update = archive_kind.is_none();
    let package_kind = archive_kind.unwrap_or(ArchivePackageKind::Full);
    let predownload_stage_dir =
        if !use_manifest_update && use_predownload && package_kind == ArchivePackageKind::Patch {
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

    if use_manifest_update {
        ui::print_info(format!(
            "Using target-manifest update with {} optional reuse source(s); patch/full archives are delivery providers rather than the update identity.",
            reuse_roots.len()
        ));
    } else {
        if patch_group_selected {
            if let Some(delta) = manifest_delta {
                ui::print_info(format!(
                    "Selected official patch as a group delivery candidate: declared patch transfer is smaller than the {} changed-file / {} direct-byte manifest upper bound.",
                    delta.changed_files,
                    delta.direct_bytes
                ));
            }
        }
        ui::print_info(describe_update_package_selection(
            &version_info,
            Some(&package_request_version),
            package_kind,
            opts.force_full_package || force_full_for_mixed_recovery,
        ));
        if !reuse_roots.is_empty() && opts.output != crate::OutputFormat::Json {
            ui::print_info(format!(
                "Keeping {} compatible source install(s) for VFS and post-update repair fallbacks.",
                reuse_roots.len()
            ));
        }
    }

    if require_staged_predownload && package_kind != ArchivePackageKind::Patch {
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
            use_manifest_update,
            &reuse_roots,
            use_predownload,
            predownload_stage_dir.as_deref(),
            opts.skip_verify,
            !opts.resource_policy.uses_resource_index(),
            opts.keep_pack_archives,
            opts.force_full_package || force_full_for_mixed_recovery,
        ) {
            opts.dry_run(line);
        }
        if require_staged_predownload {
            opts.dry_run("Would fail instead of downloading if staged predownload archives are missing or mismatched.");
        }
        return Ok(());
    }

    let selected_parts = if use_manifest_update {
        Vec::new()
    } else {
        match package_kind {
            ArchivePackageKind::Patch => version_info
                .patch
                .as_ref()
                .map(|patch| patch.patches.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|part| part.md5.clone())
                .collect(),
            ArchivePackageKind::Full => version_info
                .pkg
                .as_ref()
                .map(|pkg| pkg.packs.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|part| part.md5.clone())
                .collect(),
        }
    };
    let mut change_state = InstallChangeState::new(
        InstallChangeKind::Update,
        if use_manifest_update {
            InstallChangeSource::Manifest
        } else if package_kind == ArchivePackageKind::Patch {
            InstallChangeSource::PatchArchive
        } else {
            InstallChangeSource::FullArchive
        },
        game_id.to_string(),
        region_id.to_string(),
        channel_id.channel().to_string(),
        channel_id.sub_channel().to_string(),
        Some(package_request_version.clone()),
        version_info.version.clone(),
        version_info
            .pkg
            .as_ref()
            .and_then(|pkg| pkg.game_files_md5.clone()),
        selected_parts,
        opts.resource_policy.uses_resource_index(),
    );
    let target_manifest = &manifest_snapshot;
    let expected_archive_files = if !use_manifest_update {
        archive_expected_files(target_manifest.entries.clone())
    } else {
        archive_expected_files(Vec::new())
    };

    let mut vfs_plan = plan_update_vfs_tasks(
        api_client,
        &install_target,
        &version_info,
        &local.install_path,
        &reuse_roots,
        force_copy,
        !opts.resource_policy.uses_resource_index(),
    )
    .await?;

    change_state = change_state
        .with_game_files_path(manifest_snapshot.release.file_path.clone())
        .with_resource_identity(vfs_plan.identity.clone());

    let mut content_plan = ContentPlan::from_snapshot(
        &local.install_path,
        target_manifest.clone(),
        &vfs_plan.claims,
    )
    .context("Failed to build the update content plan")?;

    if !use_manifest_update && package_kind == ArchivePackageKind::Patch {
        validate_patch_target(&install_target.exe_name, &local.install_path).await?;
    }

    match start_install_change(&local.install_path, &change_state)? {
        InstallChangeStart::New => {}
        InstallChangeStart::Resume => ui::print_info(format!(
            "Resuming unfinished update {} -> {}",
            change_state.from_version.as_deref().unwrap_or("unknown"),
            change_state.target_version
        )),
        InstallChangeStart::Advance => ui::print_info(format!(
            "Advancing unfinished change to update {} -> {}",
            change_state.from_version.as_deref().unwrap_or("unknown"),
            change_state.target_version
        )),
    }

    let mut archive_result = ArchiveRunResult::default();
    if use_manifest_update {
        ui::print_phase("Applying target-manifest update");
        update_via_manifest(
            &local,
            &version_info,
            &content_plan,
            &reuse_roots,
            current_manifest
                .as_deref()
                .expect("manifest update requires a local manifest"),
            force_copy,
            &opts,
            task_pool_runner,
        )
        .await?;
    }

    if !use_manifest_update {
        match package_kind {
            ArchivePackageKind::Patch => {
                let patch = version_info
                    .patch
                    .as_ref()
                    .context("No patch package information available")?;
                let patch_password = patch.cd_key.as_deref();
                if let Some(stage_dir) = predownload_stage_dir.as_ref() {
                    archive_result = download_and_extract_archives_from_dir(
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
                        std::mem::take(&mut vfs_plan.tasks),
                        &vfs_plan.claims,
                        false,
                        &opts,
                        task_pool_runner,
                    )
                    .await?;
                } else {
                    archive_result = download_and_extract_archives(
                        &patch.patches,
                        &local.install_path,
                        "patch",
                        opts.keep_pack_archives,
                        patch_password,
                        &patch_options,
                        expected_archive_files.clone(),
                        std::mem::take(&mut vfs_plan.tasks),
                        &vfs_plan.claims,
                        false,
                        &opts,
                        task_pool_runner,
                    )
                    .await?;
                }
            }
            ArchivePackageKind::Full => {
                let pkg = version_info
                    .pkg
                    .as_ref()
                    .context("No full package information available")?;
                archive_result = download_and_extract_archives(
                    &pkg.packs,
                    &local.install_path,
                    "full",
                    opts.keep_pack_archives,
                    None,
                    &patch_options,
                    expected_archive_files.clone(),
                    std::mem::take(&mut vfs_plan.tasks),
                    &vfs_plan.claims,
                    true,
                    &opts,
                    task_pool_runner,
                )
                .await?;
            }
        }
    }

    let verification_selection = if use_manifest_update {
        IntegritySelection::Paths(Vec::new())
    } else if package_kind == ArchivePackageKind::Full {
        // Full-package extraction verifies and commits every archive entry as it
        // is written. The manifest closure still checks paths that were absent
        // from the archive or owned by independent file tasks, while cached
        // verified paths avoid a second read of freshly committed files.
        IntegritySelection::GameFiles
    } else {
        IntegritySelection::Paths(archive_result.modified_paths)
    };
    let post_update = verify_updated_install(
        api_client,
        &install_target.api,
        &mut content_plan,
        opts.skip_verify,
        vfs_plan,
        verification_selection,
        archive_result.verified_paths,
        &reuse_roots,
        force_copy,
        &opts,
        task_pool_runner,
    )
    .await?;
    if post_update == PostUpdateResult::Verified {
        finish_install_change(&local.install_path, &change_state)
            .context("Failed to remove the install change marker")?;
        ui::print_success("Update finished");
    } else {
        ui::print_warning(format!(
            "Update payload finished, but launcher metadata and the change marker were kept because verification was skipped. Run `griffr verify --path \"{}\" --repair` before launch.",
            local.install_path.display()
        ));
    }
    Ok(())
}

fn merge_reuse_paths(explicit: &[PathBuf], peers: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::with_capacity(explicit.len() + peers.len());
    explicit
        .iter()
        .chain(peers)
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
}

async fn plan_update_vfs_tasks(
    api_client: &ApiClient,
    install_target: &InstallTarget,
    version_info: &GetLatestGameResponse,
    install_path: &std::path::Path,
    reuse_paths: &[PathBuf],
    force_copy: bool,
    package_only: bool,
) -> Result<VfsTaskPlan> {
    if package_only {
        return Ok(VfsTaskPlan::default());
    }

    ui::print_phase("Planning VFS resources for the update DAG");
    ui::print_info(
        "VFS scope: StreamingAssets index-full (Persistent VFS setup is a separate command).",
    );
    let streaming_assets = streaming_assets_path(&install_path.join(&install_target.data_root));
    let source_streaming_assets = reuse_paths
        .iter()
        .filter(|path| path.as_path() != install_path)
        .map(|path| streaming_assets_path(&path.join(&install_target.data_root)))
        .collect::<Vec<_>>();
    let rand_str = version_info.rand_str();
    Ok(plan_vfs_tasks(
        api_client,
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
    .unwrap_or_default())
}

pub async fn update(
    paths: Vec<PathBuf>,
    overrides: crate::InstallTargetOverrideArgs,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    stage_dir: Option<PathBuf>,
    require_staged: bool,
    use_default_stage: bool,
    batch: crate::BatchArgs,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    crate::commands::batch::validate_batch_options(batch)?;
    if require_staged && stage_dir.is_none() {
        anyhow::bail!("--require-staged requires --stage-dir");
    }
    if stage_dir.is_some() && paths.len() != 1 {
        anyhow::bail!("An explicit --stage-dir can only be used with one update target");
    }

    let installs = crate::commands::batch::inspect_unique_installations(&paths).await?;
    let explicit_sources =
        crate::commands::batch::inspect_unique_reuse_sources(&reuse_paths).await?;
    let target_games = installs
        .iter()
        .map(|install| install.require_known_game())
        .collect::<Result<Vec<_>, _>>()?;
    crate::commands::batch::validate_reuse_source_games(&explicit_sources, &target_games)?;

    #[derive(Debug)]
    struct UpdateWork {
        index: usize,
        path: PathBuf,
        explicit_reuse: Vec<PathBuf>,
        peer_reuse: Vec<PathBuf>,
        volume_keys: Vec<String>,
    }

    let target_paths = installs
        .iter()
        .map(|install| install.install_path.clone())
        .collect::<Vec<_>>();
    let mut suppressed_peer_reuse = false;
    let mut work = Vec::with_capacity(installs.len());
    for (index, install) in installs.iter().enumerate() {
        let target_reuse_paths = crate::commands::batch::reuse_paths_for_target(
            &explicit_sources,
            &installs,
            &target_games,
            index,
        );
        let (mut explicit_reuse, peer_reuse) = if batch.jobs > 1 {
            suppressed_peer_reuse |= !target_reuse_paths.peers.is_empty();
            let mut explicit = target_reuse_paths.explicit;
            explicit.retain(|source| !target_paths.iter().any(|target| target == source));
            (explicit, Vec::new())
        } else {
            (target_reuse_paths.explicit, target_reuse_paths.peers)
        };
        explicit_reuse.shrink_to_fit();

        let mut volume_keys = vec![griffr_common::runtime::task_pool::storage_volume_key(
            &install.install_path,
        )];
        volume_keys.extend(
            explicit_reuse
                .iter()
                .chain(&peer_reuse)
                .map(griffr_common::runtime::task_pool::storage_volume_key),
        );
        volume_keys.extend(
            [
                stage_dir.as_ref(),
                patch_options.work_dir.as_ref(),
                patch_options.external_asset_root.as_ref(),
            ]
            .into_iter()
            .flatten()
            .map(griffr_common::runtime::task_pool::storage_volume_key),
        );
        volume_keys.sort_unstable();
        volume_keys.dedup();
        work.push(UpdateWork {
            index,
            path: install.install_path.clone(),
            explicit_reuse,
            peer_reuse,
            volume_keys,
        });
    }

    if suppressed_peer_reuse {
        ui::print_warning(
            "Concurrent target updates do not reuse from other selected targets while those targets may be changing",
        );
    }

    let api_client = ApiClient::new()?;
    let mut failures = Vec::new();
    if batch.jobs == 1 {
        let mut task_pool_runner = TaskPoolRunner::new(opts.task_pool_config())?;
        for item in work {
            let result = update_internal(
                &api_client,
                &mut task_pool_runner,
                item.path.clone(),
                overrides.clone(),
                item.explicit_reuse,
                item.peer_reuse,
                force_copy,
                use_default_stage || stage_dir.is_some(),
                patch_options.clone(),
                stage_dir.clone(),
                require_staged,
                opts,
            )
            .await;
            if let Err(error) = result {
                failures.push(crate::commands::batch::BatchFailure {
                    path: item.path,
                    error: format!("{error:#}"),
                });
                if !batch.continue_after_failure() {
                    break;
                }
            }
        }
    } else {
        let parallel_jobs = crate::commands::batch::volume_parallelism_bound(
            &work,
            batch.jobs.min(opts.batch_parallelism_limit()),
            |item| &item.volume_keys,
        );
        let (runner_group, runner_config) = opts.task_pool_batch(parallel_jobs)?;
        let mut results = crate::commands::batch::run_volume_dependency_graph(
            work,
            parallel_jobs,
            |item| &item.volume_keys,
            |item| {
                let api_client = api_client.clone();
                let runner_group = runner_group.clone();
                let runner_config = runner_config.clone();
                let overrides = overrides.clone();
                let patch_options = patch_options.clone();
                let stage_dir = stage_dir.clone();
                async move {
                    let path = item.path.clone();
                    let result = async {
                        let mut runner = runner_group.runner(runner_config)?;
                        update_internal(
                            &api_client,
                            &mut runner,
                            item.path,
                            overrides,
                            item.explicit_reuse,
                            item.peer_reuse,
                            force_copy,
                            use_default_stage || stage_dir.is_some(),
                            patch_options,
                            stage_dir,
                            require_staged,
                            opts,
                        )
                        .await
                    }
                    .await;
                    (item.index, path, result)
                }
            },
        )
        .await;
        results.sort_by_key(|(index, ..)| *index);
        for (_, path, result) in results {
            if let Err(error) = result {
                failures.push(crate::commands::batch::BatchFailure {
                    path,
                    error: format!("{error:#}"),
                });
            }
        }
    }

    crate::commands::batch::print_batch_summary("Update", installs.len(), &failures);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(crate::commands::batch::batch_error("Update", &failures))
    }
}

pub(crate) async fn apply_staged_predownload(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    predownload_dir_override: Option<PathBuf>,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    let api_client = ApiClient::new()?;
    let mut task_pool_runner = TaskPoolRunner::new(opts.task_pool_config())?;
    update_internal(
        &api_client,
        &mut task_pool_runner,
        path,
        overrides,
        Vec::new(),
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
