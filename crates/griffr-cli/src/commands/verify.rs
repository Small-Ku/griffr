use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::TaskPoolRunner;
use griffr_common::runtime::{
    finish_install_change, finish_vfs_plan, is_launcher_metadata_path, read_install_change,
    run_integrity_pool, start_install_change, sync_launcher_metadata, ContentPlan,
    GameManifestSnapshot, InstallChangeKind, InstallChangeSource, InstallChangeStart,
    InstallChangeState, IntegritySelection, ProgressLane, ProgressSender,
};
use griffr_common::runtime::{
    plan_vfs_tasks, streaming_assets_path, VfsFilePlanOptions, VfsTaskPlan,
};
use serde_json::json;
use std::path::PathBuf;

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::{GlobalOptions, OutputFormat};

async fn verify_one(
    api_client: &ApiClient,
    pool_runner: &mut TaskPoolRunner,
    local: griffr_common::runtime::LocalInstall,
    game_override: Option<GameId>,
    region_override: Option<RegionId>,
    channel_override: Option<ChannelPair>,
    overrides: crate::InstallTargetOverrideArgs,
    skip_local_detect: bool,
    repair: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    relink_reuse: bool,
    scope: Option<crate::VerifyScopeArg>,
    opts: GlobalOptions,
) -> Result<serde_json::Value> {
    let detected_game = local.game_id.as_ref();
    let detected_region = local.region_id;
    let detected_channel = local.channel_id.as_ref();
    let game_id = match game_override {
        Some(game_id) => game_id,
        None if !skip_local_detect => local.require_known_game()?,
        None => unreachable!("--skip-local-detect validation requires --game"),
    };
    let region_id = match region_override {
        Some(region_id) => region_id,
        None if !skip_local_detect => local.require_known_region()?,
        None => unreachable!("--skip-local-detect validation requires --region"),
    };
    let channel_id = match channel_override {
        Some(channel_id) => channel_id,
        None if !skip_local_detect => local.require_known_channel()?,
        None => unreachable!("--region always resolves a channel pair"),
    };
    let installed_version = local.require_config_ini_version()?.to_string();
    let pending_change = read_install_change(&local.install_path)?;
    let mut active_change = pending_change.clone();
    if let Some(state) = active_change.as_ref() {
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
    }
    let mut checked_version = active_change
        .as_ref()
        .map(|state| state.target_version.clone())
        .unwrap_or_else(|| installed_version.clone());
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let release_info = api_client
        .get_latest_game(&install_target.api, Some(&checked_version))
        .await
        .context("Failed to resolve the integrity target release")?;
    let mut advanced_change = None;
    if let Some(state) = active_change.clone() {
        let release_package = release_info.pkg.as_ref();
        let release_path = release_package.map(|pkg| pkg.file_path.as_str());
        let release_md5 = release_package.and_then(|pkg| pkg.game_files_md5.as_deref());
        if release_info.version == state.target_version {
            if !state.matches_release(&release_info.version, release_path, release_md5) {
                anyhow::bail!(
                    "Unfinished {} target {} no longer matches its saved game_files identity; refusing to use changed metadata under the same version",
                    state.kind,
                    state.target_version
                );
            }
        } else if !repair {
            anyhow::bail!(
                "Unfinished {} target {} was superseded by {}. Run verify --repair or update to advance the mixed installation to the current release.",
                state.kind,
                state.target_version,
                release_info.version
            );
        } else {
            let advanced = InstallChangeState::new(
                InstallChangeKind::Update,
                InstallChangeSource::Repair,
                game_id.to_string(),
                region_id.to_string(),
                channel_id.channel().to_string(),
                channel_id.sub_channel().to_string(),
                Some(state.target_version.clone()),
                release_info.version.clone(),
                release_package.and_then(|pkg| pkg.game_files_md5.clone()),
                Vec::new(),
                scope
                    .map(|scope| scope != crate::VerifyScopeArg::Core)
                    .unwrap_or(state.sync_vfs),
            )
            .with_game_files_path(
                release_package
                    .map(|pkg| pkg.file_path.clone())
                    .unwrap_or_default(),
            );
            checked_version = advanced.target_version.clone();
            advanced_change = Some((state.target_version.clone(), advanced.clone()));
            active_change = Some(advanced);
        }
    }

    if !skip_local_detect {
        if let Some(detected_game) = detected_game {
            if detected_game != &game_id && opts.output != OutputFormat::Json {
                ui::print_warning(format!(
                    "Overriding detected game {} with CLI --game {}",
                    detected_game, game_id
                ));
            }
        }
        if let Some(detected_region) = detected_region {
            if detected_region != region_id && opts.output != OutputFormat::Json {
                ui::print_warning(format!(
                    "Overriding detected region {} with CLI --region {}",
                    detected_region, region_id
                ));
            }
        }
        if let Some(detected_channel) = detected_channel {
            if detected_channel != &channel_id && opts.output != OutputFormat::Json {
                ui::print_warning(format!(
                    "Overriding detected channel {}/{} with CLI --channel {}/{}",
                    detected_channel.channel(),
                    detected_channel.sub_channel(),
                    channel_id.channel(),
                    channel_id.sub_channel()
                ));
            }
        }
    }

    ui::print_phase(format!(
        "Verifying {} (region={}, channel={}, sub-channel={}) at {}",
        game_id,
        region_id,
        channel_id.channel(),
        channel_id.sub_channel(),
        local.install_path.display(),
    ));
    ui::print_info(format!("Installed version: {}", installed_version));
    if let Some(state) = active_change.as_ref() {
        ui::print_warning(format!(
            "Unfinished {} change targets {}. Integrity will use the target manifest.",
            state.kind, state.target_version
        ));
    }

    let manifest_snapshot = GameManifestSnapshot::fetch(api_client, &release_info)
        .await
        .context("Failed to fetch the integrity manifest snapshot")?;

    let progress = (opts.output != OutputFormat::Json)
        .then(|| CountAndByteProgress::new("verify", "repair.download", opts.verbose));
    let progress_session = progress.as_ref().map(|progress| {
        progress.start(
            ProgressLane::INTEGRITY_VERIFY,
            ProgressLane::INTEGRITY_DOWNLOAD,
        )
    });
    let progress_sender = progress_session
        .as_ref()
        .map(|session| session.sender())
        .unwrap_or_else(ProgressSender::disabled);

    let execute_repairs = repair && !opts.is_dry_run();
    let source_roots = if execute_repairs {
        reuse_paths
    } else {
        Vec::new()
    };

    let mut effective_scope = scope.unwrap_or_else(|| {
        active_change
            .as_ref()
            .map(|state| {
                if state.sync_vfs {
                    crate::VerifyScopeArg::All
                } else {
                    crate::VerifyScopeArg::Core
                }
            })
            .unwrap_or(crate::VerifyScopeArg::All)
    });
    if active_change.as_ref().is_some_and(|state| state.sync_vfs)
        && effective_scope == crate::VerifyScopeArg::Core
    {
        effective_scope = crate::VerifyScopeArg::All;
        if opts.output != OutputFormat::Json {
            ui::print_warning(
                "Using --scope all because the unfinished change marker requires resource closure.",
            );
        }
    }
    let sync_vfs = effective_scope != crate::VerifyScopeArg::Core;
    let mut vfs_plan = if sync_vfs {
        if opts.output != OutputFormat::Json {
            ui::print_info(
                "VFS scope: StreamingAssets index-full (Persistent VFS setup is a separate command).",
            );
        }
        let rand_str = release_info.rand_str();
        if rand_str.is_empty() {
            VfsTaskPlan::default()
        } else {
            let streaming_assets =
                streaming_assets_path(&local.install_path.join(install_target.data_root.clone()));
            let source_streaming_assets = source_roots
                .iter()
                .map(|path| streaming_assets_path(&path.join(install_target.data_root.clone())))
                .collect::<Vec<_>>();
            plan_vfs_tasks(
                api_client,
                &install_target.api,
                &release_info.version,
                &rand_str,
                &streaming_assets,
                &VfsFilePlanOptions {
                    source_streaming_assets,
                    allow_repair: execute_repairs,
                    allow_copy_fallback: force_copy,
                    prefer_reuse: relink_reuse,
                },
            )
            .await
            .context("Failed to plan VFS tasks for verify+repair")?
            .unwrap_or_default()
        }
    } else {
        VfsTaskPlan::default()
    };

    if let Some((previous_target, advanced)) = advanced_change.take() {
        let advanced = advanced.with_resource_identity(vfs_plan.identity.clone());
        if opts.is_dry_run() {
            opts.dry_run(format!(
                "Would advance unfinished target {} to current release {}",
                previous_target, advanced.target_version
            ));
        } else {
            match start_install_change(&local.install_path, &advanced)? {
                InstallChangeStart::Advance => ui::print_warning(format!(
                    "Advancing unfinished target {} to current release {} during repair",
                    previous_target, advanced.target_version
                )),
                InstallChangeStart::Resume => ui::print_info(format!(
                    "Resuming repair toward current release {}",
                    advanced.target_version
                )),
                InstallChangeStart::New => unreachable!("an active marker was read above"),
            }
        }
        active_change = Some(advanced);
    } else if let Some(state) = active_change.as_ref().filter(|state| state.sync_vfs) {
        if vfs_plan.identity != state.resource_identity {
            anyhow::bail!(
                "Unfinished change target {} no longer matches its saved resource identity",
                checked_version
            );
        }
    }

    let mut content_plan =
        ContentPlan::from_snapshot(&local.install_path, manifest_snapshot, &vfs_plan.claims)
            .context("Failed to build the integrity content plan")?;

    let pool_cfg = opts.task_pool_config();
    let volume_policy = pool_cfg.default_volume_policy;
    opts.verbose(format!(
        "Volume policy: mode={:?} reads={} writes={} metadata={} pressure={} reuse_queue_limit={}",
        volume_policy.streaming_mode,
        volume_policy.read_limit,
        volume_policy.write_limit,
        volume_policy.metadata_limit,
        volume_policy.streaming_pressure_limit,
        pool_cfg.reuse_queue_limit
    ));
    if repair && !vfs_plan.tasks.is_empty() {
        opts.verbose(format!(
            "Using {} shared network slots with weighted VFS/archive fairness",
            pool_cfg.network_slots
        ));
    }
    let repair_change = if execute_repairs {
        if let Some(state) = active_change.as_ref() {
            Some(state.clone())
        } else if effective_scope == crate::VerifyScopeArg::Resources {
            None
        } else {
            let state = InstallChangeState::new(
                InstallChangeKind::Repair,
                InstallChangeSource::Repair,
                game_id.to_string(),
                region_id.to_string(),
                channel_id.channel().to_string(),
                channel_id.sub_channel().to_string(),
                Some(checked_version.clone()),
                checked_version.clone(),
                content_plan.snapshot().release.game_files_md5.clone(),
                Vec::new(),
                effective_scope != crate::VerifyScopeArg::Core,
            )
            .with_game_files_path(content_plan.snapshot().release.file_path.clone())
            .with_resource_identity(vfs_plan.identity.clone());
            match start_install_change(&local.install_path, &state)? {
                InstallChangeStart::New => {}
                InstallChangeStart::Resume => {
                    ui::print_info(format!(
                        "Resuming unfinished repair for {}",
                        checked_version
                    ));
                }
                InstallChangeStart::Advance => unreachable!("repair cannot advance a change"),
            }
            Some(state)
        }
    } else {
        None
    };
    let summary = run_integrity_pool(
        &content_plan,
        match (effective_scope, execute_repairs) {
            (crate::VerifyScopeArg::All, true) => IntegritySelection::GameFiles,
            (crate::VerifyScopeArg::All, false) => IntegritySelection::Full,
            (crate::VerifyScopeArg::Core, _) => IntegritySelection::Core,
            (crate::VerifyScopeArg::Resources, _) => IntegritySelection::Resources,
        },
        &[],
        execute_repairs,
        &source_roots,
        force_copy,
        relink_reuse,
        std::mem::take(&mut vfs_plan.tasks),
        Some(pool_runner),
        progress_sender,
    )
    .await
    .context("run_integrity_pool failed")?;
    if let Some(session) = progress_session {
        session.finish();
    }
    if let Some(progress) = progress {
        progress.finish();
    }

    let issue_list = summary
        .issues
        .iter()
        .map(|issue| {
            json!({
                "path": issue.path,
                "kind": format!("{:?}", issue.kind),
                "expected_size": issue.expected_size,
                "actual_size": issue.actual_size,
                "expected_md5": issue.expected_md5,
                "actual_md5": issue.actual_md5,
                "is_metadata": is_launcher_metadata_path(&issue.path),
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "path": local.install_path.display().to_string(),
        "game": game_id.to_string(),
        "region": region_id.to_string(),
        "channel": channel_id.channel().to_string(),
        "sub_channel": channel_id.sub_channel().to_string(),
        "version": checked_version.as_str(),
        "config_version": installed_version.as_str(),
        "pending_change": active_change.as_ref().map(|state| json!({
            "kind": state.kind.to_string(),
            "source": state.source.to_string(),
            "from_version": state.from_version.as_deref(),
            "target_version": state.target_version.as_str(),
            "sync_vfs": state.sync_vfs,
        })),
        "repair": repair,
        "dry_run": opts.is_dry_run(),
        "repairs_executed": execute_repairs,
        "scope": format!("{:?}", effective_scope).to_ascii_lowercase(),
        "issues": issue_list,
        "downloaded_files": summary.downloaded_files,
        "reused_files": summary.reused_files,
    });
    if opts.output != OutputFormat::Json {
        ui::print_info(format!("Integrity issues found: {}", summary.issues.len()));
        if execute_repairs {
            ui::print_info(format!(
                "Repair summary: downloaded={} reused={}",
                summary.downloaded_files, summary.reused_files
            ));
        } else if repair && opts.is_dry_run() {
            opts.dry_run(format!(
                "Would repair {} integrity issue(s); no files or install state were changed",
                summary.issues.len()
            ));
        }
    }

    if summary.issues.is_empty() {
        if execute_repairs {
            if effective_scope != crate::VerifyScopeArg::Core {
                finish_vfs_plan(&local.install_path, &vfs_plan, true)
                    .await
                    .context("Failed to finish the resource baseline after repair")?;
            }
            if effective_scope != crate::VerifyScopeArg::Resources {
                content_plan
                    .refresh_delivery(api_client, &install_target.api)
                    .await
                    .context("Failed to refresh launcher metadata URLs")?;
                sync_launcher_metadata(api_client, &local.install_path, content_plan.snapshot())
                    .await
                    .context("Failed to sync launcher metadata")?;
                if let Some(state) = repair_change.as_ref() {
                    finish_install_change(&local.install_path, state)
                        .context("Failed to remove the install change marker")?;
                }
            }
        } else if let Some(state) = active_change.as_ref() {
            if opts.output != OutputFormat::Json {
                ui::print_warning(format!(
                    "Target {} is valid, but the unfinished {} marker remains. Run verify --repair to sync launcher metadata and finish the change.",
                    state.target_version, state.kind
                ));
            }
        }
        return Ok(report);
    }

    if opts.output != OutputFormat::Json {
        for issue in &summary.issues {
            ui::print_warning(format!(
                "{} {:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
                issue.path,
                issue.kind,
                issue.expected_size,
                issue.actual_size,
                issue.expected_md5,
                issue.actual_md5
            ));
        }
    }

    if execute_repairs {
        let metadata_issues: Vec<_> = summary
            .issues
            .iter()
            .filter(|issue| is_launcher_metadata_path(&issue.path))
            .cloned()
            .collect();
        let remaining_non_metadata = summary
            .issues
            .iter()
            .filter(|issue| !is_launcher_metadata_path(&issue.path))
            .count();

        if !metadata_issues.is_empty() {
            ui::print_info(format!(
                "Ignored metadata-only issues: {} (launcher metadata files will be normalized)",
                metadata_issues.len()
            ));
        }
        if remaining_non_metadata > 0 {
            anyhow::bail!(
                "verify+repair stopped with {} remaining non-metadata issue(s); the install change marker was kept",
                remaining_non_metadata
            );
        }

        if effective_scope != crate::VerifyScopeArg::Core {
            finish_vfs_plan(&local.install_path, &vfs_plan, true)
                .await
                .context("Failed to finish the resource baseline after repair")?;
        }
        if effective_scope != crate::VerifyScopeArg::Resources {
            if opts.output != OutputFormat::Json {
                ui::print_phase("Syncing launcher metadata");
            }
            content_plan
                .refresh_delivery(api_client, &install_target.api)
                .await
                .context("Failed to refresh launcher metadata URLs")?;
            sync_launcher_metadata(api_client, &local.install_path, content_plan.snapshot())
                .await
                .context("Failed to sync launcher metadata after repair")?;
            if let Some(state) = repair_change.as_ref() {
                finish_install_change(&local.install_path, state)
                    .context("Failed to remove the install change marker")?;
            }
            if opts.output != OutputFormat::Json {
                ui::print_success("Launcher metadata synced");
            }
        }
    }

    if opts.output != OutputFormat::Json {
        ui::print_success(if execute_repairs {
            "Verify and repair finished"
        } else if repair && opts.is_dry_run() {
            "Verify dry-run finished"
        } else {
            "Verify finished"
        });
    }
    Ok(report)
}

pub async fn verify(
    paths: Vec<PathBuf>,
    game_override: Option<GameId>,
    region_override: Option<RegionId>,
    channel_override: Option<ChannelPair>,
    overrides: crate::InstallTargetOverrideArgs,
    skip_local_detect: bool,
    repair: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    relink_reuse: bool,
    scope: Option<crate::VerifyScopeArg>,
    opts: GlobalOptions,
) -> Result<()> {
    if relink_reuse && !repair {
        anyhow::bail!("--relink-reuse requires --repair");
    }
    if skip_local_detect
        && (game_override.is_none() || region_override.is_none() || channel_override.is_none())
    {
        anyhow::bail!("--skip-local-detect requires both --game and --region");
    }

    let installs = crate::commands::batch::inspect_unique_installations(&paths).await?;
    let explicit_sources = if repair {
        crate::commands::batch::inspect_unique_reuse_sources(&reuse_paths).await?
    } else {
        Vec::new()
    };
    let target_games = installs
        .iter()
        .map(|install| {
            if skip_local_detect {
                Ok(game_override.clone().expect("validated above"))
            } else {
                game_override
                    .clone()
                    .or_else(|| install.game_id.clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Could not determine the game for {}",
                            install.install_path.display()
                        )
                    })
            }
        })
        .collect::<Result<Vec<_>>>()?;
    crate::commands::batch::validate_reuse_source_games(&explicit_sources, &target_games)?;
    let reuse_by_target = installs
        .iter()
        .enumerate()
        .map(|(index, install)| {
            let paths = if repair {
                crate::commands::batch::reuse_paths_for_target(
                    &explicit_sources,
                    &installs,
                    &target_games,
                    index,
                )
                .all()
            } else {
                Vec::new()
            };
            if relink_reuse && paths.is_empty() {
                anyhow::bail!(
                    "--relink-reuse requires a compatible --reuse-from or another same-game --path for {}",
                    install.install_path.display()
                );
            }
            Ok(paths)
        })
        .collect::<Result<Vec<_>>>()?;

    let api_client = ApiClient::new()?;
    let mut pool_runner = TaskPoolRunner::new(opts.task_pool_config())?;
    let mut reports = Vec::with_capacity(installs.len());

    for (install, target_reuse_paths) in installs.iter().zip(reuse_by_target) {
        reports.push(
            verify_one(
                &api_client,
                &mut pool_runner,
                install.clone(),
                game_override.clone(),
                region_override,
                channel_override.clone(),
                overrides.clone(),
                skip_local_detect,
                repair,
                target_reuse_paths,
                force_copy,
                relink_reuse,
                scope,
                opts,
            )
            .await
            .with_context(|| format!("Verify failed for {}", install.install_path.display()))?,
        );
    }

    if opts.output == OutputFormat::Json {
        if reports.len() == 1 {
            ui::emit_json(&reports[0])?;
        } else {
            ui::emit_json(&json!({ "results": reports }))?;
        }
    }
    Ok(())
}
