use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::{GetLatestGameResponse, PackFile, PrePatchInfo};
use griffr_common::runtime::task_pool::{Task, TaskOutcome, TaskPoolRunner, TaskProgress};
use griffr_common::runtime::{
    get_patch_recovery_state, write_predownload_stage_metadata, PatchRecoveryState,
    PredownloadStageMetadata, ProgressLane, StagedArchivePart, DELETE_FILES_MANIFEST_NAME,
    PATCH_MANIFEST_NAME, PATCH_STAGE_DIR,
};

use crate::progress::{ArchiveProgress, CountAndByteProgress};
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::{detect_local_install, LocalInstall};

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

fn build_stage_metadata(
    local: &LocalInstall,
    pre_patch: &PrePatchInfo,
    source_version: &str,
) -> Result<PredownloadStageMetadata> {
    let game = local.require_known_game()?;
    let region = local.require_known_region()?;
    let channel = local.require_known_channel()?;
    let archives = pre_patch
        .patches
        .iter()
        .map(|patch| {
            Ok(StagedArchivePart {
                filename: patch
                    .filename()
                    .context("Failed to extract predownload archive filename")?
                    .to_string(),
                md5: patch.md5.clone(),
                size: patch.size(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PredownloadStageMetadata {
        schema_version: PredownloadStageMetadata::SCHEMA_VERSION,
        game: game.to_string(),
        region: region.to_string(),
        channel: channel.channel().to_string(),
        sub_channel: channel.sub_channel().to_string(),
        source_version: source_version.to_string(),
        target_version: pre_patch.version.clone(),
        archives,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

fn build_predownload_tasks(stage_dir: &Path, patches: &[PackFile]) -> Result<Vec<Task>> {
    let mut tasks = Vec::with_capacity(patches.len());
    for patch in patches {
        let filename = patch
            .filename()
            .context("Failed to extract predownload archive filename")?
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
                transfer_class: griffr_common::runtime::task_pool::TransferClass::General,
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
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &Default::default(),
    )?;
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(&install_target.api, Some(&current_version))
        .await?;

    let pre_patch = version_info
        .pre_patch
        .as_ref()
        .filter(|pre_patch| !pre_patch.patches.is_empty())
        .cloned()
        .context("No predownload payload is available.")?;

    Ok((local, api_client, version_info, pre_patch, current_version))
}

async fn print_predownload_status(
    path: &Path,
) -> Result<(LocalInstall, GetLatestGameResponse, PrePatchInfo, String)> {
    let (local, _api_client, version_info, pre_patch, current_version) =
        resolve_predownload_payload(path).await?;
    let game_id = local.require_known_game()?;
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let stage_dir = stage_dir_for_request(
        &local.install_path,
        &version_info,
        &current_version,
        &pre_patch.version,
    );

    ui::print_phase(format!(
        "Checking predownload for {} (region={}, channel={}, sub-channel={}) at {}",
        game_id,
        region_id,
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

    let task_pool_cfg = opts.task_pool_config();
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;
    let tasks = build_predownload_tasks(&stage_dir, &pre_patch.patches)?;

    let progress =
        CountAndByteProgress::new("predownload.verify", "predownload.download", opts.verbose);
    let verify_lane = ProgressLane::PREDOWNLOAD_VERIFY;
    let download_lane = ProgressLane::PREDOWNLOAD_DOWNLOAD;
    let progress_session = progress.start(verify_lane, download_lane);
    let task_progress = TaskProgress::new(progress_session.sender())
        .with_verify(verify_lane, tasks.len())
        .with_download(download_lane);
    let result = task_pool_runner.run_batch(tasks, task_progress)?;
    progress_session.finish();
    progress.finish();

    let downloaded_parts = result
        .outcomes
        .iter()
        .filter(|event| matches!(event, TaskOutcome::Downloaded { .. }))
        .count();
    let failures = result
        .outcomes
        .iter()
        .filter_map(|event| match event {
            TaskOutcome::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
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

    let metadata = build_stage_metadata(&local, &pre_patch, &current_version)?;
    write_predownload_stage_metadata(&stage_dir, &metadata)
        .context("Failed to persist predownload stage metadata")?;

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
    overrides: crate::InstallTargetOverrideArgs,
    output_dir: Option<PathBuf>,
    patch_options: griffr_common::runtime::PatchApplyOptions,
    opts: GlobalOptions,
) -> Result<()> {
    if let Some(output_dir) = output_dir.as_ref() {
        ui::print_info(format!(
            "Using explicit predownload stage dir: {}",
            output_dir.display()
        ));
    }
    super::update::apply_staged_predownload(path, overrides, output_dir, patch_options, opts).await
}

pub async fn resume(path: PathBuf, opts: GlobalOptions) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let install_root = local.install_path;
    let initial_task = match get_patch_recovery_state(&install_root, None)? {
        PatchRecoveryState::ExtractedReady => Task::ApplyExtractedVfsPatchManifest {
            install_root: install_root.clone(),
        },
        PatchRecoveryState::DeletePending => Task::ApplyDeleteManifest {
            install_root: install_root.clone(),
        },
        PatchRecoveryState::ExtractedIncomplete { missing } => anyhow::bail!(
            "Extracted patch state is incomplete under {}. Missing recoverable payload/base for: {}",
            install_root.display(),
            missing.join(", ")
        ),
        PatchRecoveryState::Inconsistent { reasons } => anyhow::bail!(
            "Extracted patch state is inconsistent under {}: {}",
            install_root.display(),
            reasons.join("; ")
        ),
        PatchRecoveryState::ArchiveReady { stage_dir } => anyhow::bail!(
            "Archive-only state at {} must be applied with `predownload apply --output-dir`, not resume.",
            stage_dir.display()
        ),
        PatchRecoveryState::Complete => anyhow::bail!(
            "No extracted local patch state found under {} (expected {}, {}, or {}).",
            install_root.display(),
            PATCH_MANIFEST_NAME,
            PATCH_STAGE_DIR,
            DELETE_FILES_MANIFEST_NAME
        ),
    };

    ui::print_phase(format!(
        "Resuming local extracted patch state at {}",
        install_root.display()
    ));

    let task_pool_cfg = opts.task_pool_config();
    let mut task_pool_runner = TaskPoolRunner::new(task_pool_cfg)?;
    let progress = ArchiveProgress::new("predownload.resume", opts.verbose);
    let verify_lane = ProgressLane::ARCHIVE_VERIFY;
    let download_lane = ProgressLane::ARCHIVE_DOWNLOAD;
    let extract_lane = ProgressLane::ARCHIVE_EXTRACT;
    let commit_lane = ProgressLane::ARCHIVE_COMMIT;
    let patch_lane = ProgressLane::ARCHIVE_PATCH;
    let delete_lane = ProgressLane::ARCHIVE_DELETE;
    let progress_session = progress.start(
        verify_lane,
        download_lane,
        extract_lane,
        commit_lane,
        patch_lane,
        delete_lane,
    );
    let task_progress = TaskProgress::new(progress_session.sender())
        .with_commit(commit_lane)
        .with_patch(patch_lane)
        .with_delete(delete_lane);
    let result = task_pool_runner.run_batch(vec![initial_task], task_progress)?;
    progress_session.finish();
    progress.finish();

    let failures = result
        .outcomes
        .into_iter()
        .filter_map(|event| match event {
            TaskOutcome::Failed { path, reason } => Some(format!("{} ({})", path, reason)),
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
