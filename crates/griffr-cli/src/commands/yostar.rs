use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_core::{BackendKind, GameId, RegionId};
use griffr_runtime::task_pool::TaskPoolRunner;
use griffr_runtime::{
    cleanup_yostar_obsolete_files, ensure_yostar_files_with_pool, finish_install_change,
    start_install_change, verify_yostar_files_with_pool, write_yostar_metadata, InstallChangeKind,
    InstallChangeSource, InstallChangeStart, InstallChangeState, LocalInstall, ProgressLane,
};
use griffr_yostar_api::{YostarApiClient, YostarReleaseSnapshot};
use serde_json::json;

use crate::progress::CountAndByteProgress;
use crate::{ui, GlobalOptions, InstallTargetOverrideArgs, OutputFormat};

fn validate_target(game: &GameId, region: RegionId) -> Result<()> {
    if game != &GameId::ARKNIGHTS || region.backend() != BackendKind::Yostar {
        anyhow::bail!(
            "YoStar backend supports Arknights regions kr, en, and jp; got --game {game} --region {region}"
        );
    }
    Ok(())
}

fn client(region: RegionId, overrides: &InstallTargetOverrideArgs) -> Result<YostarApiClient> {
    if overrides.api.game_appcode.is_some() || overrides.api.launcher_appcode.is_some() {
        anyhow::bail!(
            "YoStar {region} does not use Hypergryph --game-appcode/--launcher-appcode overrides"
        );
    }
    if overrides.exe_name.is_some() || overrides.data_root.is_some() {
        anyhow::bail!(
            "YoStar {region} obtains the executable from native launcher metadata; --exe/--data-root are not applicable"
        );
    }
    YostarApiClient::arknights_with_gateway(region, overrides.api.gateway.as_deref())
        .map_err(Into::into)
}

fn change_state(
    region: RegionId,
    kind: InstallChangeKind,
    from_version: Option<String>,
    target_version: &str,
    basis: &str,
) -> InstallChangeState {
    InstallChangeState::new(
        kind,
        if kind == InstallChangeKind::Repair {
            InstallChangeSource::Repair
        } else {
            InstallChangeSource::Manifest
        },
        GameId::ARKNIGHTS.to_string(),
        region.to_string(),
        "",
        "",
        from_version,
        target_version.to_string(),
        None,
        Vec::new(),
        false,
    )
    // `game_files_path` is the generic persisted manifest locator in the
    // change receipt. For YoStar the corresponding release identity is its
    // observed `basis`.
    .with_game_files_path(basis)
}

fn report_change_start(start: InstallChangeStart, kind: &str, version: &str) {
    match start {
        InstallChangeStart::New => {}
        InstallChangeStart::Resume => {
            ui::print_info(format!(
                "Resuming unfinished YoStar {kind} for target {version}"
            ));
        }
        InstallChangeStart::Advance => {
            ui::print_info(format!(
                "Advancing unfinished YoStar {kind} to target {version}"
            ));
        }
    }
}

fn cdn_roots(release: &YostarReleaseSnapshot) -> Vec<String> {
    let mut roots = vec![release.cdn.primary_cdn.clone()];
    if !release.cdn.back_up_cdn.trim().is_empty()
        && release.cdn.back_up_cdn != release.cdn.primary_cdn
    {
        roots.push(release.cdn.back_up_cdn.clone());
    }
    roots
}

#[allow(clippy::too_many_arguments)]
pub async fn install(
    game_id: GameId,
    region_id: RegionId,
    install_path: PathBuf,
    install_path_had_entries: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    overrides: InstallTargetOverrideArgs,
    opts: GlobalOptions,
) -> Result<()> {
    validate_target(&game_id, region_id)?;
    if opts.keep_pack_archives {
        anyhow::bail!("YoStar's observed updater exposes raw per-file CDN delivery, not retained package archives");
    }
    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would install YoStar {} region={} into {} with CRC64-XZ verification",
            game_id,
            region_id,
            install_path.display()
        ));
        return Ok(());
    }

    compio::fs::create_dir_all(&install_path)
        .await
        .with_context(|| format!("Failed to create {}", install_path.display()))?;

    let client = client(region_id, &overrides)?;
    let release = client
        .latest_release()
        .await
        .context("Failed to resolve the latest YoStar release")?;
    griffr_runtime::validate_remote_yostar_manifest(&release.manifest)?;

    ui::print_phase(format!(
        "Installing {} (YoStar {}) into {}",
        game_id,
        region_id,
        install_path.display()
    ));
    ui::print_info(format!(
        "Target version: {} | Minimum launchable: {} | Files: {} | Logical size: {}",
        release.config.game_latest_version,
        release.config.game_lowest_version,
        release.manifest.files.len(),
        ui::format_bytes(release.manifest.files.iter().map(|file| file.size).sum())
    ));

    let change = change_state(
        region_id,
        InstallChangeKind::Install,
        None,
        &release.config.game_latest_version,
        &release.config.game_latest_file_path,
    );
    report_change_start(
        start_install_change(&install_path, &change)?,
        "install",
        &release.config.game_latest_version,
    );

    let progress = CountAndByteProgress::new("install.verify", "install.download", opts.verbose);
    let session = progress.start(
        ProgressLane::FILE_ENSURE_VERIFY,
        ProgressLane::FILE_ENSURE_DOWNLOAD,
    );
    let mut runner = TaskPoolRunner::new(opts.task_pool_config())?;
    let summary = ensure_yostar_files_with_pool(
        &install_path,
        None,
        &release.manifest,
        &cdn_roots(&release),
        &reuse_paths,
        force_copy,
        !install_path_had_entries,
        false,
        &mut runner,
        session.sender(),
    )
    .await
    .context("Failed to materialize YoStar game files")?;
    session.finish();
    progress.finish();
    if !summary.issues.is_empty() {
        anyhow::bail!(
            "YoStar install left {} unresolved file issue(s)",
            summary.issues.len()
        );
    }

    write_yostar_metadata(&install_path, region_id, &release.config, &release.manifest)
        .context("Failed to commit YoStar launcher metadata")?;
    finish_install_change(&install_path, &change)
        .context("Failed to finalize YoStar install change")?;
    ui::print_success(format!(
        "Installed YoStar {} {} (downloaded={}, reused={})",
        game_id, release.config.game_latest_version, summary.downloaded_files, summary.reused_files
    ));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    local: &LocalInstall,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    stage_requested: bool,
    require_staged: bool,
    overrides: InstallTargetOverrideArgs,
    opts: GlobalOptions,
    runner: &mut TaskPoolRunner,
) -> Result<()> {
    let metadata = local
        .yostar_metadata()
        .context("YoStar update requires YoStar local metadata")?;
    let region = local.require_known_region()?;
    validate_target(&GameId::ARKNIGHTS, region)?;
    if stage_requested || require_staged || opts.force_full_package || opts.keep_pack_archives {
        anyhow::bail!("YoStar {region} does not expose the Hypergryph archive/patch staging controls used by this update invocation");
    }

    let client = client(region, &overrides)?;
    let (release_result, current_result) = futures_util::join!(
        client.latest_release(),
        client.manifest_for(metadata.version(), metadata.basis())
    );
    let release = release_result.context("Failed to resolve the latest YoStar release")?;
    let current = match current_result {
        Ok(manifest) => {
            griffr_runtime::validate_remote_yostar_manifest(&manifest)?;
            Some(manifest)
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "Could not resolve the canonical current YoStar manifest; falling back to a conservative target materialization and preserving obsolete files"
            );
            None
        }
    };

    ui::print_phase(format!(
        "Updating Arknights (YoStar {}) at {}",
        region,
        local.install_path.display()
    ));
    ui::print_info(format!(
        "Current version: {} | Latest: {} | Minimum launchable: {}",
        metadata.version(),
        release.config.game_latest_version,
        release.config.game_lowest_version
    ));

    if opts.is_dry_run() {
        let changed = release
            .manifest
            .files
            .iter()
            .filter(|target| {
                current.as_ref().is_none_or(|current| {
                    current
                        .files
                        .iter()
                        .find(|old| {
                            griffr_runtime::normalize_logical_path(&old.path)
                                == griffr_runtime::normalize_logical_path(&target.path)
                        })
                        .is_none_or(|old| old.size != target.size || old.hash != target.hash)
                })
            })
            .count();
        opts.dry_run(format!(
            "Would manifest-update to {} ({} changed/new target files)",
            release.config.game_latest_version, changed
        ));
        return Ok(());
    }

    let change = change_state(
        region,
        InstallChangeKind::Update,
        Some(metadata.version().to_string()),
        &release.config.game_latest_version,
        &release.config.game_latest_file_path,
    );
    report_change_start(
        start_install_change(&local.install_path, &change)?,
        "update",
        &release.config.game_latest_version,
    );

    let blocking = if let Some(current) = current.as_ref() {
        cleanup_yostar_obsolete_files(&local.install_path, current, &release.manifest, true).await?
    } else {
        Default::default()
    };
    if blocking.removed_files > 0 {
        ui::print_info(format!(
            "Removed {} obsolete blocking file(s)",
            blocking.removed_files
        ));
    }

    let progress = CountAndByteProgress::new("update.verify", "update.download", opts.verbose);
    let session = progress.start(
        ProgressLane::FILE_ENSURE_VERIFY,
        ProgressLane::FILE_ENSURE_DOWNLOAD,
    );
    let summary = ensure_yostar_files_with_pool(
        &local.install_path,
        current.as_ref(),
        &release.manifest,
        &cdn_roots(&release),
        &reuse_paths,
        force_copy,
        false,
        false,
        runner,
        session.sender(),
    )
    .await
    .context("Failed to materialize YoStar update")?;
    session.finish();
    progress.finish();
    if !summary.issues.is_empty() {
        anyhow::bail!(
            "YoStar update left {} unresolved file issue(s)",
            summary.issues.len()
        );
    }

    let cleanup = if let Some(current) = current.as_ref() {
        cleanup_yostar_obsolete_files(&local.install_path, current, &release.manifest, false)
            .await?
    } else {
        ui::print_warning(
            "Canonical current YoStar manifest was unavailable; preserved obsolete files rather than deleting paths without server-backed ownership proof",
        );
        Default::default()
    };
    if cleanup.retained_modified_files > 0 {
        ui::print_warning(format!(
            "Preserved {} modified obsolete file(s) not owned safely by the new release",
            cleanup.retained_modified_files
        ));
    }
    write_yostar_metadata(
        &local.install_path,
        region,
        &release.config,
        &release.manifest,
    )
    .context("Failed to commit YoStar target metadata")?;
    finish_install_change(&local.install_path, &change)
        .context("Failed to finalize YoStar update change")?;
    ui::print_success(format!(
        "YoStar update complete: {} -> {} (downloaded={}, reused={}, obsolete_removed={})",
        metadata.version(),
        release.config.game_latest_version,
        summary.downloaded_files,
        summary.reused_files,
        blocking.removed_files + cleanup.removed_files
    ));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    local: &LocalInstall,
    game_override: Option<GameId>,
    region_override: Option<RegionId>,
    repair: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    scope: Option<crate::VerifyScopeArg>,
    overrides: InstallTargetOverrideArgs,
    opts: GlobalOptions,
    runner: &mut TaskPoolRunner,
) -> Result<serde_json::Value> {
    if game_override
        .as_ref()
        .is_some_and(|game| game != &GameId::ARKNIGHTS)
    {
        anyhow::bail!("YoStar install is Arknights; incompatible --game override");
    }
    let region = local.require_known_region()?;
    if region_override.is_some_and(|override_region| override_region != region) {
        anyhow::bail!("YoStar install uses region {region}; incompatible --region override");
    }
    if scope == Some(crate::VerifyScopeArg::Resources) {
        anyhow::bail!("YoStar {region} has no Hypergryph launcher resource-index/VFS scope; use --scope core or all");
    }
    let metadata = local.yostar_metadata().context("missing YoStar metadata")?;
    let api = client(region, &overrides)?;

    let text = opts.output != OutputFormat::Json;
    if text {
        ui::print_phase(format!(
            "Verifying Arknights (YoStar {}) at {}",
            region,
            local.install_path.display()
        ));
        ui::print_info(format!(
            "Installed version: {} | Files: {}",
            metadata.version(),
            metadata.manifest.files.len()
        ));
    }
    if opts.is_dry_run() && repair {
        opts.dry_run("Would CRC64-XZ verify every target file and repair failures from reuse/CDN");
        return Ok(
            json!({"backend":"yostar","region":region.to_string(),"version":metadata.version(),"repair":true,"dry_run":true}),
        );
    }

    let (manifest, roots) = if repair {
        let (manifest, cdn) = futures_util::try_join!(
            api.manifest_for(metadata.version(), metadata.basis()),
            api.cdn_config()
        )
        .context("Failed to resolve YoStar repair manifest/CDN")?;
        griffr_runtime::validate_remote_yostar_manifest(&manifest)?;
        let mut roots = vec![cdn.primary_cdn];
        if !cdn.back_up_cdn.trim().is_empty() && cdn.back_up_cdn != roots[0] {
            roots.push(cdn.back_up_cdn);
        }
        (manifest, roots)
    } else {
        // The native local manifest is protected by the launcher's observed vc
        // scheme and was validated during local-install detection. Full verify
        // can therefore remain offline; only repair needs a remote source.
        (metadata.manifest.as_content_manifest(""), Vec::new())
    };

    let repair_change = if repair {
        let state = change_state(
            region,
            InstallChangeKind::Repair,
            Some(metadata.version().to_string()),
            metadata.version(),
            metadata.basis(),
        );
        report_change_start(
            start_install_change(&local.install_path, &state)?,
            "repair",
            metadata.version(),
        );
        Some(state)
    } else {
        None
    };

    let progress = CountAndByteProgress::new("verify", "repair.download", opts.verbose);
    let session = progress.start(
        ProgressLane::INTEGRITY_VERIFY,
        ProgressLane::INTEGRITY_DOWNLOAD,
    );
    let summary = verify_yostar_files_with_pool(
        &local.install_path,
        &manifest,
        repair,
        &roots,
        &reuse_paths,
        force_copy,
        runner,
        session.sender(),
    )
    .await
    .context("YoStar integrity run failed")?;
    session.finish();
    progress.finish();

    if repair && summary.issues.is_empty() {
        finish_install_change(
            &local.install_path,
            repair_change
                .as_ref()
                .expect("repair change exists when repair is enabled"),
        )
        .context("Failed to finalize YoStar repair change")?;
    }

    if text {
        ui::print_info(format!("Integrity issues found: {}", summary.issues.len()));
        for issue in &summary.issues {
            ui::print_warning(format!(
                "{} {:?} expected_size={} actual_size={:?} expected_hash={} actual_hash={:?}",
                issue.path,
                issue.kind,
                issue.expected_size,
                issue.actual_size,
                issue.expected_hash.manifest_value(),
                issue.actual_hash.as_ref().map(|hash| hash.manifest_value())
            ));
        }
        if repair && summary.issues.is_empty() {
            ui::print_success("YoStar verify and repair finished");
        } else if summary.issues.is_empty() {
            ui::print_success("YoStar verify finished");
        }
    }

    Ok(json!({
        "backend": "yostar",
        "region": region.to_string(),
        "version": metadata.version(),
        "repair": repair,
        "issues": summary.issues,
        "downloaded_files": summary.downloaded_files,
        "reused_files": summary.reused_files,
    }))
}
