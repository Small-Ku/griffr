use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::game::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::game::{plan_vfs_tasks, VfsMaterializeConfig, VfsTaskPlan};
use serde_json::json;

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::ui;
use crate::{GlobalOptions, OutputFormat};

fn is_launcher_metadata_issue(path: &str) -> bool {
    matches!(
        path.replace('\\', "/").to_ascii_lowercase().as_str(),
        "game_files" | "package_files"
    )
}

pub async fn verify(
    path: PathBuf,
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

    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let server_id = local.require_known_server()?;
    let installed_version = local.require_config_ini_version()?.to_string();
    let manager = local.as_manager()?;
    let api_client = ApiClient::new()?;

    ui::print_phase(format!(
        "Verifying {} ({}) at {}",
        game_id,
        server_id,
        local.install_path.display(),
    ));
    ui::print_info(format!("Installed version: {}", installed_version));

    let progress_cb = if opts.output == OutputFormat::Json {
        None
    } else {
        let verify_bar = Arc::new(StepProgress::new(
            if repair { "verify+repair" } else { "verify" },
            opts.verbose,
        ));
        let verify_bar_cb = verify_bar.clone();
        Some((
            verify_bar,
            move |current: usize, total: usize, file: &str| {
                verify_bar_cb.update(current, total, file);
            },
        ))
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
                    game_id
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
            .get_latest_game(game_id, server_id, Some(&installed_version))
            .await
            .context("Failed to fetch version information for VFS planning")?;
        let rand_str = version_info.rand_str();
        if rand_str.is_empty() {
            Vec::new()
        } else {
            let streaming_assets = local
                .install_path
                .join(game_id.streaming_assets_subdir())
                .join("StreamingAssets");
            let source_streaming_assets = source_roots
                .iter()
                .map(|path| {
                    path.join(game_id.streaming_assets_subdir())
                        .join("StreamingAssets")
                })
                .collect::<Vec<_>>();
            let VfsTaskPlan { tasks, .. } = plan_vfs_tasks(
                &api_client,
                game_id,
                server_id,
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
            .context("Failed to plan VFS tasks for verify+repair")?;
            tasks
        }
    } else {
        Vec::new()
    };

    let mut pool_cfg = TaskPoolConfig::default();
    pool_cfg.max_retries = 3;
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
            progress_cb.as_ref().map(|(_, cb)| cb),
        )
        .await
        .context("run_integrity_pool failed")?;
    if let Some((bar, _)) = progress_cb {
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
                    "is_metadata": is_launcher_metadata_issue(&issue.path),
                })
            })
            .collect::<Vec<_>>();
        ui::emit_json(&json!({
            "path": local.install_path.display().to_string(),
            "game": game_id.to_string(),
            "server": server_id.to_string(),
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
            .filter(|issue| is_launcher_metadata_issue(&issue.path))
            .cloned()
            .collect();
        let remaining_non_metadata = summary
            .issues
            .iter()
            .filter(|issue| !is_launcher_metadata_issue(&issue.path))
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
