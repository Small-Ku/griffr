use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::{GetLatestGameResponse, PackFile, PrePatchInfo};
use griffr_common::runtime::task_pool::{ProgressEvent, Task, TaskPoolConfig, TaskPoolRunner};

use super::local::{detect_local_install, LocalInstall};
use crate::progress::{ByteProgressTracker, StepProgress};
use crate::ui;
use crate::GlobalOptions;

pub(crate) fn default_stage_dir(
    install_path: &Path,
    request_version: &str,
    target_version: &str,
) -> PathBuf {
    install_path
        .join("downloads")
        .join("predownload")
        .join(format!("{}-{}_file", request_version, target_version))
}

pub(crate) fn stage_dir_for_request(
    install_path: &Path,
    version_info: &GetLatestGameResponse,
    current_version: &str,
    target_version: &str,
) -> PathBuf {
    let request_version = if version_info.request_version.is_empty() {
        current_version
    } else {
        version_info.request_version.as_str()
    };
    default_stage_dir(install_path, request_version, target_version)
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

async fn resolve_predownload_payload(
    path: &Path,
) -> Result<(
    LocalInstall,
    ApiClient,
    GetLatestGameResponse,
    PrePatchInfo,
    String,
)> {
    let local = detect_local_install(path).await?;
    let current_version = local.require_config_ini_version()?.to_string();

    let game_id = local.require_known_game()?;
    let channel_id = local.require_known_channel()?;
    let profile =
        griffr_common::config::resolve_install_profile(&game_id, &channel_id, &Default::default())?;
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(&profile.target, Some(&current_version))
        .await?;

    let pre_patch = version_info
        .pre_patch
        .as_ref()
        .filter(|pre_patch| !pre_patch.patches.is_empty())
        .cloned()
        .context("No predownload payload is currently available.")?;

    Ok((local, api_client, version_info, pre_patch, current_version))
}

async fn print_predownload_status(
    path: &Path,
) -> Result<(LocalInstall, GetLatestGameResponse, PrePatchInfo, String)> {
    let (local, _api_client, version_info, pre_patch, current_version) =
        resolve_predownload_payload(path).await?;
    let game_id = local.require_known_game()?;
    let channel_id = local.require_known_channel()?;
    let stage_dir = stage_dir_for_request(
        &local.install_path,
        &version_info,
        &current_version,
        &pre_patch.version,
    );

    ui::print_phase(format!(
        "Checking predownload for {} (channel={}, sub-channel={}) at {}",
        game_id,
        channel_id.channel(),
        channel_id.sub_channel(),
        local.install_path.display()
    ));
    ui::print_info(format!(
        "Current version (config.ini): {} | Remote version: {}",
        current_version, version_info.version
    ));
    ui::print_info(format!(
        "Predownload target: {} | Parts: {} | Size: {}",
        pre_patch.version,
        pre_patch.patches.len(),
        ui::format_bytes(predownload_total_size(&pre_patch))
    ));
    ui::print_info(format!("Stage dir: {}", stage_dir.display()));

    Ok((local, version_info, pre_patch, current_version))
}

pub async fn check(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let _ = print_predownload_status(&path).await?;
    Ok(())
}

pub async fn fetch(path: PathBuf, output_dir: Option<PathBuf>, opts: GlobalOptions) -> Result<()> {
    let (local, version_info, pre_patch, current_version) = print_predownload_status(&path).await?;
    let stage_dir = output_dir.unwrap_or_else(|| {
        stage_dir_for_request(
            &local.install_path,
            &version_info,
            &current_version,
            &pre_patch.version,
        )
    });

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

    let task_pool_cfg = TaskPoolConfig {
        max_retries: 3,
        download_progress_buffer_bytes: opts.download_progress_buffer_bytes,
        ..Default::default()
    };
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;
    let tasks = build_predownload_tasks(&stage_dir, &pre_patch.patches)?;

    let total_size = predownload_total_size(&pre_patch);
    let bar = StepProgress::new("predownload.download", opts.verbose);
    let mut progress = ByteProgressTracker::new(bar.clone(), total_size).log_verified_in_verbose();
    let result = task_pool_runner.run_batch_with_progress(
        tasks,
        Some(&mut |event: &ProgressEvent| progress.handle_event(event)),
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

pub async fn apply(
    path: PathBuf,
    overrides: crate::InstallProfileOverrideArgs,
    output_dir: Option<PathBuf>,
    opts: GlobalOptions,
) -> Result<()> {
    if let Some(output_dir) = output_dir.as_ref() {
        ui::print_info(format!(
            "Using explicit predownload stage dir: {}",
            output_dir.display()
        ));
    }
    super::update::apply_staged_predownload(path, overrides, output_dir, opts).await
}

pub async fn resume(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let install_root = local.install_path;
    let patch_manifest = install_root.join("patch.json");
    let patch_stage_dir = install_root.join("vfs_files");
    let delete_manifest = install_root.join("delete_files.txt");

    let initial_task = if patch_manifest.is_file() || patch_stage_dir.exists() {
        Task::ApplyExtractedVfsPatchManifest {
            install_root: install_root.clone(),
        }
    } else if delete_manifest.is_file() {
        Task::ApplyDeleteManifest {
            install_root: install_root.clone(),
        }
    } else {
        anyhow::bail!(
            "No extracted local patch state found under {} (expected patch.json, vfs_files, or delete_files.txt).",
            install_root.display()
        );
    };

    ui::print_phase(format!(
        "Resuming local extracted patch state at {}",
        install_root.display()
    ));

    let task_pool_cfg = TaskPoolConfig {
        max_retries: 3,
        ..Default::default()
    };
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;
    let result = task_pool_runner.run_batch_with_progress(vec![initial_task], None)?;

    let failures = result
        .events
        .into_iter()
        .filter_map(|event| match event {
            ProgressEvent::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        anyhow::bail!(
            "Local patch resume failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    ui::print_success("Local extracted patch state resumed");
    Ok(())
}
