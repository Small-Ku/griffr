use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{ChannelPair, GameId};
use griffr_common::runtime::is_launcher_metadata_path;
use griffr_common::runtime::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::runtime::{plan_vfs_tasks, VfsMaterializeConfig};
use serde_json::json;
use std::path::PathBuf;

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::ui;
use crate::{GlobalOptions, OutputFormat};

pub async fn verify(
    path: PathBuf,
    game_override: Option<GameId>,
    channel_override: Option<ChannelPair>,
    overrides: crate::InstallProfileOverrideArgs,
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
    if skip_local_detect && (game_override.is_none() || channel_override.is_none()) {
        anyhow::bail!("--skip-local-detect requires both --game and --channel");
    }

    let local = detect_local_install(&path).await?;
    let detected_game = local.game_id.as_ref();
    let detected_channel = local.channel_id.as_ref();
    let game_id = if skip_local_detect {
        game_override.expect("validated above")
    } else {
        game_override.unwrap_or(local.require_known_game()?)
    };
    let channel_id = if skip_local_detect {
        channel_override.expect("validated above")
    } else {
        channel_override.unwrap_or(local.require_known_channel()?)
    };
    let installed_version = local.require_config_ini_version()?.to_string();
    let profile = griffr_common::config::resolve_install_profile(
        &game_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let manager = local.as_manager(profile.clone())?;
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
        "Verifying {} (channel={}, sub-channel={}) at {}",
        game_id,
        channel_id.channel(),
        channel_id.sub_channel(),
        local.install_path.display(),
    ));
    ui::print_info(format!("Installed version: {}", installed_version));

    let progress_cb = if opts.output == OutputFormat::Json {
        None
    } else {
        let verify_bar = StepProgress::new(
            if repair { "verify+repair" } else { "verify" },
            opts.verbose,
        );
        let (cb1, cb2) = verify_bar.split_callbacks();
        Some((verify_bar, cb1, cb2))
    };

    let mut source_roots = Vec::new();
    if repair {
        for reuse_path in &reuse_paths {
            let source = detect_local_install(reuse_path).await.with_context(|| {
                format!("Failed to inspect reuse source {}", reuse_path.display())
            })?;
            let source_game_id = source.require_known_game()?;
            if source_game_id != game_id {
                anyhow::bail!(
                    "Reuse source {} is {:?}, expected {:?}",
                    source.install_path.display(),
                    source_game_id,
                    &game_id
                );
            }
            if source.install_path != local.install_path {
                source_roots.push(source.install_path.clone());
            }
        }
    }

    let extra_tasks = if repair && !skip_vfs {
        if opts.output != OutputFormat::Json {
            ui::print_info(
                "VFS scope: StreamingAssets index-full (Persistent bootstrap is a separate step).",
            );
        }
        let version_info = api_client
            .get_latest_game(&profile.target, Some(&installed_version))
            .await
            .context("Failed to fetch version information for VFS planning")?;
        let rand_str = version_info.rand_str();
        if rand_str.is_empty() {
            Vec::new()
        } else {
            let streaming_assets = local
                .install_path
                .join(profile.streaming_assets_subdir.clone())
                .join("StreamingAssets");
            let source_streaming_assets = source_roots
                .iter()
                .map(|path| {
                    path.join(profile.streaming_assets_subdir.clone())
                        .join("StreamingAssets")
                })
                .collect::<Vec<_>>();
            match plan_vfs_tasks(
                &api_client,
                &profile.target,
                &version_info.version,
                &rand_str,
                &streaming_assets,
                &VfsMaterializeConfig {
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

    let mut pool_cfg = TaskPoolConfig {
        max_retries: 3,
        extraction_progress_buffer_bytes: opts.extraction_progress_buffer_bytes,
        download_progress_buffer_bytes: opts.download_progress_buffer_bytes,
        ..Default::default()
    };
    if repair && !extra_tasks.is_empty() {
        // VFS CDN endpoints can become unstable under high parallelism on some routes.
        // Keep a moderate IO fanout for repair+VFS runs to improve success rate.
        let clamped = pool_cfg.io_slots.min(6);
        if clamped != pool_cfg.io_slots {
            opts.verbose(format!(
                "Clamping task-pool io_slots from {} to {} for verify+repair VFS batch",
                pool_cfg.io_slots, clamped
            ));
            pool_cfg.io_slots = clamped;
        }
    }
    let mut pool_runner = TaskPoolRunner::new(pool_cfg)?;
    let summary = manager
        .run_integrity_pool_with_runner(
            &api_client,
            repair,
            &source_roots,
            force_copy,
            relink_reuse,
            extra_tasks,
            Some(&mut pool_runner),
            progress_cb.as_ref().map(|(_, cb, _)| cb),
            progress_cb.as_ref().map(|(_, _, cb)| cb),
        )
        .await
        .context("run_integrity_pool failed")?;
    if let Some((bar, _, _)) = progress_cb {
        bar.finish();
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
            manager
                .sync_launcher_metadata(&api_client)
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
        manager
            .sync_launcher_metadata(&api_client)
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
