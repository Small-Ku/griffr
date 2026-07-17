use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId, RegionId};
use griffr_common::runtime::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::runtime::{
    inspect_reuse_installations, is_launcher_metadata_path, run_integrity_pool,
    sync_launcher_metadata, IntegritySelection, ProgressLane, ProgressSender,
};
use griffr_common::runtime::{plan_vfs_tasks, streaming_assets_path, VfsFilePlanOptions};
use serde_json::json;
use std::path::PathBuf;

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::{GlobalOptions, OutputFormat};
use griffr_common::runtime::detect_local_install;

pub async fn verify(
    path: PathBuf,
    game_override: Option<GameId>,
    region_override: Option<RegionId>,
    channel_override: Option<ChannelPair>,
    overrides: crate::InstallTargetOverrideArgs,
    skip_local_detect: bool,
    repair: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    relink_reuse: bool,
    skip_vfs: bool,
    opts: GlobalOptions,
) -> Result<()> {
    if relink_reuse && !repair {
        anyhow::bail!("--relink-reuse requires --repair");
    }
    if relink_reuse && reuse_paths.is_empty() {
        anyhow::bail!("--relink-reuse requires at least one --reuse-from source");
    }
    if skip_local_detect && (game_override.is_none() || region_override.is_none()) {
        anyhow::bail!("--skip-local-detect requires both --game and --region");
    }

    let local = detect_local_install(&path).await?;
    let detected_game = local.game_id.as_ref();
    let detected_region = local.region_id;
    let detected_channel = local.channel_id.as_ref();
    let game_id = if skip_local_detect {
        game_override.expect("validated above")
    } else {
        game_override.unwrap_or(local.require_known_game()?)
    };
    let region_id = if skip_local_detect {
        region_override.expect("validated above")
    } else {
        region_override.unwrap_or(local.require_known_region()?)
    };
    let channel_id = if skip_local_detect {
        channel_override.expect("validated above")
    } else {
        channel_override.unwrap_or(local.require_known_channel()?)
    };
    let installed_version = local.require_config_ini_version()?.to_string();
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let api_client = ApiClient::new()?;

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

    let source_roots = if repair {
        inspect_reuse_installations(&game_id, &local.install_path, &reuse_paths)
            .await?
            .into_iter()
            .map(|source| source.install_path)
            .collect()
    } else {
        Vec::new()
    };

    let extra_tasks = if repair && !skip_vfs {
        if opts.output != OutputFormat::Json {
            ui::print_info(
                "VFS scope: StreamingAssets index-full (Persistent bootstrap is a separate step).",
            );
        }
        let version_info = api_client
            .get_latest_game(&install_target.api, Some(&installed_version))
            .await
            .context("Failed to fetch version information for VFS planning")?;
        let rand_str = version_info.rand_str();
        if rand_str.is_empty() {
            Vec::new()
        } else {
            let streaming_assets =
                streaming_assets_path(&local.install_path.join(install_target.data_root.clone()));
            let source_streaming_assets = source_roots
                .iter()
                .map(|path| streaming_assets_path(&path.join(install_target.data_root.clone())))
                .collect::<Vec<_>>();
            match plan_vfs_tasks(
                &api_client,
                &install_target.api,
                &version_info.version,
                &rand_str,
                &streaming_assets,
                &VfsFilePlanOptions {
                    source_streaming_assets,
                    allow_copy_fallback: force_copy,
                    prefer_reuse: relink_reuse,
                },
            )
            .await
            .context("Failed to plan VFS tasks for verify+repair")?
            {
                griffr_common::runtime::VfsPlanOutcome::Planned(plan) => plan.tasks,
                griffr_common::runtime::VfsPlanOutcome::Unsupported => Vec::new(),
            }
        }
    } else {
        Vec::new()
    };

    let pool_cfg = TaskPoolConfig::with_progress_buffers(
        opts.extraction_progress_buffer_bytes,
        opts.download_progress_buffer_bytes,
    );
    if repair && !extra_tasks.is_empty() {
        opts.verbose(format!(
            "Using {} shared network slots with weighted VFS/archive fairness",
            pool_cfg.network_slots
        ));
    }
    let mut pool_runner = TaskPoolRunner::new(pool_cfg)?;
    let summary = run_integrity_pool(
        &api_client,
        &local.install_path,
        &install_target,
        Some(&installed_version),
        IntegritySelection::Full,
        repair,
        &source_roots,
        force_copy,
        relink_reuse,
        extra_tasks,
        Some(&mut pool_runner),
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

    if opts.output == OutputFormat::Json {
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
        ui::emit_json(&json!({
            "path": local.install_path.display().to_string(),
            "game": game_id.to_string(),
            "region": region_id.to_string(),
            "channel": channel_id.channel().to_string(),
            "sub_channel": channel_id.sub_channel().to_string(),
            "version": installed_version,
            "repair": repair,
            "issues": issue_list,
            "downloaded_files": summary.downloaded_files,
            "reused_files": summary.reused_files,
        }))?;
    } else {
        ui::print_info(format!("Integrity issues found: {}", summary.issues.len()));
        if repair {
            ui::print_info(format!(
                "Repair summary: downloaded={} reused={}",
                summary.downloaded_files, summary.reused_files
            ));
        }
    }

    if summary.issues.is_empty() {
        if repair {
            sync_launcher_metadata(
                &api_client,
                &local.install_path,
                &install_target,
                Some(&installed_version),
            )
            .await
            .context("Failed to sync launcher metadata")?;
        }
        return Ok(());
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

    if repair {
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
        if opts.output != OutputFormat::Json {
            ui::print_phase("Syncing launcher metadata");
        }
        sync_launcher_metadata(
            &api_client,
            &local.install_path,
            &install_target,
            Some(&installed_version),
        )
        .await
        .context("Failed to sync launcher metadata after repair")?;
        if opts.output != OutputFormat::Json {
            ui::print_success("Launcher metadata synced");
        }

        if remaining_non_metadata > 0 {
            anyhow::bail!(
                "verify+repair finished with {} remaining non-metadata issue(s)",
                remaining_non_metadata
            );
        }
    }

    if opts.output != OutputFormat::Json {
        ui::print_success(if repair {
            "Verify+repair complete"
        } else {
            "Verify complete"
        });
    }
    Ok(())
}
