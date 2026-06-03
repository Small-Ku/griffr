use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::{PackFile, PrePatchInfo};
use griffr_common::runtime::task_pool::{ProgressEvent, Task, TaskPoolConfig, TaskPoolRunner};

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::ui;
use crate::GlobalOptions;

fn default_stage_dir(install_path: &Path, request_version: &str, target_version: &str) -> PathBuf {
    install_path
        .join("downloads")
        .join("predownload")
        .join(format!("{}-{}_file", request_version, target_version))
}

fn predownload_total_size(pre_patch: &PrePatchInfo) -> u64 {
    pre_patch
        .package_size
        .parse::<u64>()
        .unwrap_or_else(|_| pre_patch.patches.iter().map(PackFile::size).sum())
}

fn build_predownload_tasks(stage_dir: &Path, patches: &[PackFile]) -> Result<Vec<Task>> {
    let mut tasks = Vec::with_capacity(patches.len());
    for patch in patches {
        let filename = patch
            .filename()
            .context("Failed to extract predownload archive filename")?
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let dest = stage_dir.join(&filename);
        tasks.push(Task::Verify {
            path: dest.clone(),
            logical_path: filename.clone(),
            expected_md5: patch.md5.clone(),
            expected_size: Some(patch.size()),
            on_fail: Some(Box::new(Task::Download {
                url: patch.url.clone(),
                dest,
                logical_path: filename,
                expected_md5: patch.md5.clone(),
                expected_size: Some(patch.size()),
                retry_count: 0,
            })),
        });
    }
    Ok(tasks)
}

pub async fn predownload(
    path: PathBuf,
    check_only: bool,
    output_dir: Option<PathBuf>,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let server_id = local.require_known_server()?;
    let current_version = local.require_config_ini_version()?.to_string();

    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(game_id, server_id, Some(&current_version))
        .await?;

    ui::print_phase(format!(
        "Checking predownload for {} ({}) at {}",
        game_id,
        server_id,
        local.install_path.display()
    ));
    ui::print_info(format!(
        "Current version (config.ini): {} | Remote version: {}",
        current_version, version_info.version
    ));

    let pre_patch = match version_info.pre_patch.as_ref() {
        Some(pre_patch) if !pre_patch.patches.is_empty() => pre_patch,
        _ => {
            ui::print_info("No predownload payload is currently available.");
            return Ok(());
        }
    };

    let request_version = if version_info.request_version.is_empty() {
        current_version.as_str()
    } else {
        version_info.request_version.as_str()
    };
    let stage_dir = output_dir.unwrap_or_else(|| {
        default_stage_dir(&local.install_path, request_version, &pre_patch.version)
    });
    let total_size = predownload_total_size(pre_patch);

    ui::print_info(format!(
        "Predownload target: {} | Parts: {} | Size: {}",
        pre_patch.version,
        pre_patch.patches.len(),
        ui::format_bytes(total_size)
    ));
    ui::print_info(format!("Stage dir: {}", stage_dir.display()));

    if check_only {
        return Ok(());
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would stage {} predownload archive part(s) into {}",
            pre_patch.patches.len(),
            stage_dir.display()
        ));
        return Ok(());
    }

    compio::fs::create_dir_all(&stage_dir)
        .await
        .with_context(|| format!("Failed to create {}", stage_dir.display()))?;

    let mut task_pool_cfg = TaskPoolConfig::default();
    task_pool_cfg.max_retries = 3;
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;
    let tasks = build_predownload_tasks(&stage_dir, &pre_patch.patches)?;

    let bar = Arc::new(StepProgress::new("predownload.download", opts.verbose));
    let bar_cb = bar.clone();
    let mut downloaded_bytes = 0u64;
    let result = task_pool_runner.run_batch_with_progress(
        tasks,
        Some(&mut |event: &ProgressEvent| match event {
            ProgressEvent::Downloaded { path, bytes } => {
                downloaded_bytes = downloaded_bytes.saturating_add(*bytes).min(total_size);
                bar_cb.update_bytes(downloaded_bytes, total_size, path);
            }
            ProgressEvent::Verified { path, ok, .. } if *ok => {
                if opts.verbose {
                    ui::print_info(format!("Verified {}", path));
                }
            }
            _ => {}
        }),
    )?;
    bar.finish();

    let downloaded_parts = result
        .events
        .iter()
        .filter(|event| matches!(event, ProgressEvent::Downloaded { .. }))
        .count();
    let failures = result
        .events
        .iter()
        .filter_map(|event| match event {
            ProgressEvent::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        anyhow::bail!(
            "Predownload staging failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    ui::print_success(format!(
        "Predownload staged: target={} parts={} downloaded_now={} stage_dir={}",
        pre_patch.version,
        pre_patch.patches.len(),
        downloaded_parts,
        stage_dir.display()
    ));

    Ok(())
}
