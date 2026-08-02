use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use futures_util::{stream, StreamExt, TryStreamExt};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::GetLatestGameResponse;
use griffr_common::config::InstallTarget;
use griffr_common::runtime::task_pool::{archive_expected_files, TaskPoolRunner};
use griffr_common::runtime::{
    finish_install_change, plan_vfs_tasks, read_install_change, read_local_game_files,
    resolve_staged_patch_recovery_dir, select_update_package, start_install_change,
    streaming_assets_path, ContentPlan, GameManifestSnapshot, InstallChangeKind,
    InstallChangeSource, InstallChangeStart, InstallChangeState, IntegritySelection,
    UpdatePackageKind, VfsFilePlanOptions, VfsTaskPlan,
};

use super::*;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

const PEER_INSPECTION_CONCURRENCY: usize = 8;

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
    let refreshed_peers = stream::iter(peer_reuse_paths.iter())
        .map(|source_path| async move {
            detect_local_install(source_path)
                .await
                .with_context(|| format!("Failed to refresh reuse peer {}", source_path.display()))
        })
        .buffered(PEER_INSPECTION_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    let target_version_peer_roots = refreshed_peers
        .into_iter()
        .filter(|source| install_can_source_target(source, &version_info.version))
        .map(|source| source.install_path)
        .collect::<Vec<_>>();

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
    let package_kind = if opts.force_full_package || force_full_for_mixed_recovery {
        UpdatePackageKind::Full
    } else {
        select_update_package(&version_info, Some(&package_request_version))?
    };
    let reuse_update_requested =
        !explicit_reuse_paths.is_empty() || !target_version_peer_roots.is_empty();
    let current_reuse_manifest = if reuse_update_requested {
        match read_local_game_files(&local.install_path).await {
            Ok(manifest) => manifest,
            Err(error) => {
                if opts.output != crate::OutputFormat::Json {
                    ui::print_warning(format!(
                        "Local game_files metadata cannot be used for safe reuse cleanup ({error}); falling back to the selected archive update."
                    ));
                }
                None
            }
        }
    } else {
        None
    };
    let staged_patch_requested = use_predownload && package_kind == UpdatePackageKind::Patch;
    let use_reuse_update = should_use_reuse_update(
        reuse_update_requested,
        current_reuse_manifest.is_some(),
        require_staged_predownload,
        opts.force_full_package,
        staged_patch_requested,
    );
    let reuse_update_roots = if use_reuse_update {
        reuse_roots.clone()
    } else {
        Vec::new()
    };
    if !reuse_roots.is_empty() && !use_reuse_update && opts.output != crate::OutputFormat::Json {
        let reason = if opts.force_full_package
            || (force_full_for_mixed_recovery && !reuse_update_requested)
        {
            "The full package is required"
        } else if staged_patch_requested || require_staged_predownload {
            "Staged predownload archives were requested"
        } else if reuse_update_requested && current_reuse_manifest.is_none() {
            "The current launcher game_files manifest is unavailable for safe obsolete-file cleanup"
        } else {
            "No automatic peer is already at the target version"
        };
        ui::print_info(format!(
            "{reason}; keep the selected archive update and use reuse sources only for VFS and repair fallbacks."
        ));
    }
    let predownload_stage_dir =
        if !use_reuse_update && use_predownload && package_kind == UpdatePackageKind::Patch {
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

    if use_reuse_update {
        ui::print_info(
            "Using manifest-driven local file reuse; archive package selection is bypassed.",
        );
    } else {
        ui::print_info(describe_update_package_selection(
            &version_info,
            Some(&package_request_version),
            package_kind,
            opts.force_full_package || force_full_for_mixed_recovery,
        ));
    }
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
            &reuse_update_roots,
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

    let selected_parts = if use_reuse_update {
        Vec::new()
    } else {
        match package_kind {
            UpdatePackageKind::Patch => version_info
                .patch
                .as_ref()
                .map(|patch| patch.patches.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|part| part.md5.clone())
                .collect(),
            UpdatePackageKind::Full => version_info
                .pkg
                .as_ref()
                .map(|pkg| pkg.packs.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|part| part.md5.clone())
                .collect(),
        }
    };
    let manifest_snapshot = GameManifestSnapshot::fetch(api_client, &version_info)
        .await
        .context("Failed to fetch the update manifest snapshot")?;

    let mut change_state = InstallChangeState::new(
        InstallChangeKind::Update,
        if use_reuse_update {
            InstallChangeSource::Reuse
        } else if package_kind == UpdatePackageKind::Patch {
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
    let expected_archive_files = if !use_reuse_update {
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

    if !use_reuse_update && package_kind == UpdatePackageKind::Patch {
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
    if use_reuse_update {
        ui::print_phase("Applying update via local file reuse");
        update_via_reuse(
            &local,
            &version_info,
            &content_plan,
            &reuse_update_roots,
            current_reuse_manifest
                .as_deref()
                .expect("reuse update requires a local manifest"),
            force_copy,
            &opts,
            task_pool_runner,
        )
        .await?;
    }

    if !use_reuse_update {
        match package_kind {
            UpdatePackageKind::Patch => {
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
            UpdatePackageKind::Full => {
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

    let verification_selection = if !use_reuse_update && package_kind == UpdatePackageKind::Full {
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

fn install_can_source_target(
    source: &griffr_common::runtime::LocalInstall,
    target_version: &str,
) -> bool {
    source.require_config_ini_version().ok() == Some(target_version)
        || read_install_change(&source.install_path)
            .ok()
            .flatten()
            .is_some_and(|state| state.target_version == target_version)
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
    use_predownload: bool,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    let installs = crate::commands::batch::inspect_unique_installations(&paths).await?;
    let explicit_sources =
        crate::commands::batch::inspect_unique_reuse_sources(&reuse_paths).await?;
    let target_games = installs
        .iter()
        .map(|install| install.require_known_game())
        .collect::<Result<Vec<_>, _>>()?;
    crate::commands::batch::validate_reuse_source_games(&explicit_sources, &target_games)?;
    let api_client = ApiClient::new()?;
    let mut task_pool_runner = TaskPoolRunner::new(opts.task_pool_config())?;

    for (index, install) in installs.iter().enumerate() {
        let target_reuse_paths = crate::commands::batch::reuse_paths_for_target(
            &explicit_sources,
            &installs,
            &target_games,
            index,
        );
        update_internal(
            &api_client,
            &mut task_pool_runner,
            install.install_path.clone(),
            overrides.clone(),
            target_reuse_paths.explicit,
            target_reuse_paths.peers,
            force_copy,
            use_predownload,
            patch_options.clone(),
            None,
            false,
            opts,
        )
        .await
        .with_context(|| format!("Update failed for {}", install.install_path.display()))?;
    }
    Ok(())
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
