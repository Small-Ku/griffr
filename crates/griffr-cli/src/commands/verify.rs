use anyhow::{Context, Result};
use futures_util::{stream, StreamExt};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::TaskPoolRunner;
use griffr_common::runtime::{
    finish_install_change, finish_vfs_plan, is_launcher_metadata_path, read_install_change,
    run_integrity_pool, start_install_change, sync_launcher_metadata, ContentPlan, FileIssue,
    GameManifestSnapshot, InstallChangeKind, InstallChangeSource, InstallChangeStart,
    InstallChangeState, IntegritySelection, ProgressLane, ProgressSender,
};
use griffr_common::runtime::{
    plan_vfs_tasks, streaming_assets_path, VfsFilePlanOptions, VfsTaskPlan,
};
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::{GlobalOptions, OutputFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RepairClosure {
    metadata_issues: usize,
    non_metadata_issues: usize,
}

impl RepairClosure {
    fn from_issues(issues: &[FileIssue]) -> Self {
        let metadata_issues = issues
            .iter()
            .filter(|issue| is_launcher_metadata_path(&issue.path))
            .count();
        Self {
            metadata_issues,
            non_metadata_issues: issues.len().saturating_sub(metadata_issues),
        }
    }

    const fn is_satisfied(self) -> bool {
        self.non_metadata_issues == 0
    }
}

#[derive(Debug)]
struct RepairTransaction {
    enabled: bool,
    scope: crate::VerifyScopeArg,
    change: Option<InstallChangeState>,
    prepared: bool,
    file_dag_completed: bool,
    closure: Option<RepairClosure>,
    committed: bool,
}

impl RepairTransaction {
    fn prepare(
        enabled: bool,
        scope: crate::VerifyScopeArg,
        active_change: Option<&InstallChangeState>,
        install_path: &Path,
        game_id: &GameId,
        region_id: RegionId,
        channel_id: &ChannelPair,
        checked_version: &str,
        content_plan: &ContentPlan,
        vfs_plan: &VfsTaskPlan,
        text_output: bool,
    ) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                enabled: false,
                scope,
                change: None,
                prepared: false,
                file_dag_completed: false,
                closure: None,
                committed: false,
            });
        }

        let change = if let Some(state) = active_change {
            Some(state.clone())
        } else if scope == crate::VerifyScopeArg::Resources {
            None
        } else {
            let state = InstallChangeState::new(
                InstallChangeKind::Repair,
                InstallChangeSource::Repair,
                game_id.to_string(),
                region_id.to_string(),
                channel_id.channel().to_string(),
                channel_id.sub_channel().to_string(),
                Some(checked_version.to_string()),
                checked_version.to_string(),
                content_plan.snapshot().release.game_files_md5.clone(),
                Vec::new(),
                scope != crate::VerifyScopeArg::Core,
            )
            .with_game_files_path(content_plan.snapshot().release.file_path.clone())
            .with_resource_identity(vfs_plan.identity.clone());
            let start = start_install_change(install_path, &state)?;
            if text_output {
                match start {
                    InstallChangeStart::New => {}
                    InstallChangeStart::Resume => {
                        ui::print_info(format!("Resuming unfinished repair for {checked_version}"))
                    }
                    InstallChangeStart::Advance => unreachable!("repair cannot advance a change"),
                }
            }
            Some(state)
        };

        Ok(Self {
            enabled: true,
            scope,
            change,
            prepared: true,
            file_dag_completed: false,
            closure: None,
            committed: false,
        })
    }

    fn complete_file_dag(&mut self, issues: &[FileIssue]) -> RepairClosure {
        let closure = RepairClosure::from_issues(issues);
        self.file_dag_completed = true;
        self.closure = Some(closure);
        closure
    }

    async fn commit(
        &mut self,
        api_client: &ApiClient,
        install_path: &Path,
        install_target: &griffr_common::config::InstallTarget,
        content_plan: &mut ContentPlan,
        vfs_plan: &VfsTaskPlan,
        text_output: bool,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let closure = self
            .closure
            .context("repair transaction closure was not evaluated")?;
        if !closure.is_satisfied() {
            anyhow::bail!(
                "verify+repair stopped with {} remaining non-metadata issue(s); the install change marker was kept",
                closure.non_metadata_issues
            );
        }

        if self.scope != crate::VerifyScopeArg::Core {
            finish_vfs_plan(install_path, vfs_plan, true)
                .await
                .context("Failed to finish the resource baseline after repair")?;
        }
        if self.scope != crate::VerifyScopeArg::Resources {
            if text_output {
                ui::print_phase("Syncing launcher metadata");
            }
            content_plan
                .refresh_delivery(api_client, &install_target.api)
                .await
                .context("Failed to refresh launcher metadata URLs")?;
            sync_launcher_metadata(api_client, install_path, content_plan.snapshot())
                .await
                .context("Failed to sync launcher metadata after repair")?;
            if let Some(state) = self.change.as_ref() {
                finish_install_change(install_path, state)
                    .context("Failed to remove the install change marker")?;
            }
            if text_output {
                ui::print_success("Launcher metadata synced");
            }
        }
        self.committed = true;
        Ok(())
    }

    fn report(&self) -> serde_json::Value {
        let closure = self.closure;
        json!({
            "enabled": self.enabled,
            "prepared": self.prepared,
            "file_dag_completed": self.file_dag_completed,
            "closure_satisfied": closure.map(RepairClosure::is_satisfied),
            "metadata_issues": closure.map(|value| value.metadata_issues),
            "non_metadata_issues": closure.map(|value| value.non_metadata_issues),
            "committed": self.committed,
        })
    }
}

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
    let text_output = opts.output != OutputFormat::Json;
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
            if detected_game != &game_id && text_output {
                ui::print_warning(format!(
                    "Overriding detected game {} with CLI --game {}",
                    detected_game, game_id
                ));
            }
        }
        if let Some(detected_region) = detected_region {
            if detected_region != region_id && text_output {
                ui::print_warning(format!(
                    "Overriding detected region {} with CLI --region {}",
                    detected_region, region_id
                ));
            }
        }
        if let Some(detected_channel) = detected_channel {
            if detected_channel != &channel_id && text_output {
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

    if text_output {
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
    }

    let manifest_snapshot = GameManifestSnapshot::fetch(api_client, &release_info)
        .await
        .context("Failed to fetch the integrity manifest snapshot")?;

    let progress =
        (text_output).then(|| CountAndByteProgress::new("verify", "repair.download", opts.verbose));
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
        if text_output {
            ui::print_warning(
                "Using --scope all because the unfinished change marker requires resource closure.",
            );
        }
    }
    let sync_vfs = effective_scope != crate::VerifyScopeArg::Core;
    let mut vfs_plan = if sync_vfs {
        if text_output {
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
            let start = start_install_change(&local.install_path, &advanced)?;
            if text_output {
                match start {
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
    let mut repair_transaction = RepairTransaction::prepare(
        execute_repairs,
        effective_scope,
        active_change.as_ref(),
        &local.install_path,
        &game_id,
        region_id,
        &channel_id,
        &checked_version,
        &content_plan,
        &vfs_plan,
        text_output,
    )?;
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

    if text_output {
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

    let closure = repair_transaction.complete_file_dag(&summary.issues);
    if execute_repairs && closure.metadata_issues > 0 && text_output {
        ui::print_info(format!(
            "Metadata-only issues to normalize: {}",
            closure.metadata_issues
        ));
    }
    if !execute_repairs && summary.issues.is_empty() {
        if let Some(state) = active_change.as_ref() {
            if text_output {
                ui::print_warning(format!(
                    "Target {} is valid, but the unfinished {} marker remains. Run verify --repair to sync launcher metadata and finish the change.",
                    state.target_version, state.kind
                ));
            }
        }
    }

    repair_transaction
        .commit(
            api_client,
            &local.install_path,
            &install_target,
            &mut content_plan,
            &vfs_plan,
            text_output,
        )
        .await?;

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
        "transaction": repair_transaction.report(),
        "issues": issue_list,
        "downloaded_files": summary.downloaded_files,
        "reused_files": summary.reused_files,
    });

    if text_output {
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
    batch: crate::BatchArgs,
    opts: GlobalOptions,
) -> Result<()> {
    crate::commands::batch::validate_batch_options(batch)?;
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

    #[derive(Debug)]
    struct VerifyWork {
        index: usize,
        install: griffr_common::runtime::LocalInstall,
        reuse_paths: Vec<PathBuf>,
        volume_keys: Vec<String>,
    }

    let target_paths = installs
        .iter()
        .map(|install| install.install_path.clone())
        .collect::<Vec<_>>();
    let mut suppressed_peer_reuse = false;
    let mut work = Vec::with_capacity(installs.len());
    for (index, install) in installs.iter().enumerate() {
        let mut reuse = if repair {
            let target_reuse = crate::commands::batch::reuse_paths_for_target(
                &explicit_sources,
                &installs,
                &target_games,
                index,
            );
            if batch.jobs > 1 {
                suppressed_peer_reuse |= !target_reuse.peers.is_empty();
                target_reuse
                    .explicit
                    .into_iter()
                    .filter(|source| !target_paths.iter().any(|target| target == source))
                    .collect::<Vec<_>>()
            } else {
                target_reuse.all()
            }
        } else {
            Vec::new()
        };
        reuse.sort_unstable();
        reuse.dedup();
        if relink_reuse && reuse.is_empty() {
            anyhow::bail!(
                "--relink-reuse requires a stable compatible --reuse-from{} for {}",
                if batch.jobs > 1 {
                    " when --jobs is greater than 1"
                } else {
                    " or another same-game --path"
                },
                install.install_path.display()
            );
        }

        let mut volume_keys = vec![griffr_common::runtime::task_pool::storage_volume_key(
            &install.install_path,
        )];
        volume_keys.extend(
            reuse
                .iter()
                .map(griffr_common::runtime::task_pool::storage_volume_key),
        );
        volume_keys.sort_unstable();
        volume_keys.dedup();
        work.push(VerifyWork {
            index,
            install: install.clone(),
            reuse_paths: reuse,
            volume_keys,
        });
    }

    if suppressed_peer_reuse {
        ui::print_warning(
            "Concurrent repairs do not reuse from other selected targets while those targets may be changing",
        );
    }

    let api_client = ApiClient::new()?;
    let mut reports = vec![None; installs.len()];
    let mut failures = Vec::new();
    if batch.jobs == 1 {
        let mut pool_runner = TaskPoolRunner::new(opts.task_pool_config())?;
        for item in work {
            let path = item.install.install_path.clone();
            match verify_one(
                &api_client,
                &mut pool_runner,
                item.install,
                game_override.clone(),
                region_override,
                channel_override.clone(),
                overrides.clone(),
                skip_local_detect,
                repair,
                item.reuse_paths,
                force_copy,
                relink_reuse,
                scope,
                opts,
            )
            .await
            {
                Ok(report) => reports[item.index] = Some(report),
                Err(error) => {
                    failures.push(crate::commands::batch::BatchFailure {
                        path,
                        error: format!("{error:#}"),
                    });
                    if !batch.continue_after_failure() {
                        break;
                    }
                }
            }
        }
    } else {
        let waves = crate::commands::batch::plan_disjoint_volume_waves(work, batch.jobs, |item| {
            &item.volume_keys
        });
        for wave in waves {
            let mut results = stream::iter(wave)
                .map(|item| {
                    let api_client = api_client.clone();
                    let game_override = game_override.clone();
                    let channel_override = channel_override.clone();
                    let overrides = overrides.clone();
                    async move {
                        let path = item.install.install_path.clone();
                        let result = async {
                            let mut runner =
                                TaskPoolRunner::new(opts.task_pool_config_for_batch(batch.jobs))?;
                            verify_one(
                                &api_client,
                                &mut runner,
                                item.install,
                                game_override,
                                region_override,
                                channel_override,
                                overrides,
                                skip_local_detect,
                                repair,
                                item.reuse_paths,
                                force_copy,
                                relink_reuse,
                                scope,
                                opts,
                            )
                            .await
                        }
                        .await;
                        (item.index, path, result)
                    }
                })
                .buffer_unordered(batch.jobs)
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(index, ..)| *index);
            for (index, path, result) in results {
                match result {
                    Ok(report) => reports[index] = Some(report),
                    Err(error) => failures.push(crate::commands::batch::BatchFailure {
                        path,
                        error: format!("{error:#}"),
                    }),
                }
            }
        }
    }

    if opts.output == OutputFormat::Json {
        if installs.len() == 1 && failures.is_empty() {
            ui::emit_json(reports[0].as_ref().expect("single successful report"))?;
        } else {
            let results = installs
                .iter()
                .enumerate()
                .map(|(index, install)| {
                    if let Some(report) = reports[index].as_ref() {
                        json!({
                            "path": install.install_path,
                            "status": "ok",
                            "report": report,
                        })
                    } else {
                        let error = failures
                            .iter()
                            .find(|failure| failure.path == install.install_path)
                            .map(|failure| failure.error.as_str())
                            .unwrap_or("not run after fail-fast");
                        json!({
                            "path": install.install_path,
                            "status": "error",
                            "error": error,
                        })
                    }
                })
                .collect::<Vec<_>>();
            ui::emit_json(&json!({
                "results": results,
                "summary": {
                    "total": installs.len(),
                    "succeeded": reports.iter().filter(|report| report.is_some()).count(),
                    "failed": failures.len(),
                }
            }))?;
        }
    } else {
        crate::commands::batch::print_batch_summary("Verify", installs.len(), &failures);
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(crate::commands::batch::batch_error("Verify", &failures))
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use griffr_common::runtime::FileIssueKind;

    fn issue(path: &str) -> FileIssue {
        FileIssue {
            path: path.to_string(),
            expected_md5: "0".repeat(32),
            expected_size: 1,
            actual_size: None,
            actual_md5: None,
            kind: FileIssueKind::Missing,
        }
    }

    #[test]
    fn repair_closure_accepts_clean_and_metadata_only_results() {
        assert!(RepairClosure::from_issues(&[]).is_satisfied());
        let closure = RepairClosure::from_issues(&[issue("config.ini")]);
        assert!(closure.is_satisfied());
        assert_eq!(closure.metadata_issues, 1);
        assert_eq!(closure.non_metadata_issues, 0);
    }

    #[test]
    fn repair_closure_blocks_remaining_payload_issues() {
        let closure = RepairClosure::from_issues(&[
            issue("config.ini"),
            issue("Endfield_Data/StreamingAssets/data.bin"),
        ]);
        assert!(!closure.is_satisfied());
        assert_eq!(closure.metadata_issues, 1);
        assert_eq!(closure.non_metadata_issues, 1);
    }
}
