use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::runtime::task_pool::TaskPoolRunner;
use griffr_common::runtime::{
    inspect_reuse_installations, setup_persistent_vfs, Launcher, PersistentVfsConfig,
    PersistentVfsFileSet, ProgressLane,
};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;
use griffr_common::runtime::detect_local_install;

pub async fn setup_persistent_resources(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    file_set: PersistentVfsFileSet,
    reuse_paths: Vec<PathBuf>,
    allow_download: bool,
    prefer_reuse: bool,
    prune_extra_files: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    if local.is_yostar() {
        anyhow::bail!(
            "YoStar EN does not use the Hypergryph Persistent/VFS resource-index workflow"
        );
    }
    let game_id = local.require_known_game()?;
    let region_id = local.require_known_region()?;
    let channel_id = local.require_known_channel()?;
    let installed_version = local.require_config_ini_version()?.to_string();

    let install_target = griffr_common::config::resolve_install_target(
        &game_id,
        region_id,
        &channel_id,
        &overrides.clone().into(),
    )?;
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(&install_target.api, Some(&installed_version))
        .await
        .context("Failed to get version data for Persistent resource setup")?;
    if version_info.version != installed_version {
        anyhow::bail!(
            "Persistent resources can only be prepared for the installed version {}. The launcher reports target version {}; update the game first",
            installed_version,
            version_info.version
        );
    }

    let rand_str = version_info.rand_str();
    if rand_str.is_empty() {
        anyhow::bail!(
            "Could not resolve rand_str for {} (region={}, channel={}, sub-channel={}) version {}",
            game_id,
            region_id,
            channel_id.channel(),
            channel_id.sub_channel(),
            installed_version
        );
    }

    let data_root = local.install_path.join(install_target.data_root.clone());
    let streaming_assets_root = griffr_common::runtime::streaming_assets_path(&data_root);
    let persistent_root = griffr_common::runtime::persistent_path(&data_root);

    let reuse_sources =
        inspect_reuse_installations(&game_id, &local.install_path, &reuse_paths).await?;
    let mut extra_source_streaming_assets = Vec::with_capacity(reuse_sources.len());
    for source in reuse_sources {
        let source_target = griffr_common::config::resolve_install_target(
            &source.require_known_game()?,
            source.require_known_region()?,
            &source.require_known_channel()?,
            &Default::default(),
        )?;
        extra_source_streaming_assets.push(
            source
                .install_path
                .join(source_target.data_root)
                .join(griffr_common::runtime::STREAMING_ASSETS_DIR),
        );
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would set up Persistent resources for {} (region={}, channel={}, sub-channel={}) at {} with file_set={:?}",
            game_id,
            region_id,
            channel_id.channel(),
            channel_id.sub_channel(),
            local.install_path.display(),
            file_set
        ));
        opts.dry_run(format!(
            "Would use source StreamingAssets: {}",
            streaming_assets_root.display()
        ));
        if !extra_source_streaming_assets.is_empty() {
            opts.dry_run(format!(
                "Would use other StreamingAssets sources: {}",
                extra_source_streaming_assets
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        opts.dry_run(format!(
            "Would check and write files in Persistent: {}",
            persistent_root.display()
        ));
        opts.dry_run(format!(
            "allow_download={} prefer_reuse={} prune_extra_files={} copy_only=true",
            allow_download, prefer_reuse, prune_extra_files
        ));
        return Ok(());
    }

    let launcher = Launcher::new(
        game_id.clone(),
        install_target.clone(),
        local.install_path.clone(),
    );
    if launcher.is_game_running() {
        anyhow::bail!(
            "Cannot change Persistent resources while {} is running from {}",
            game_id,
            local.install_path.display()
        );
    }

    ui::print_phase(format!(
        "Setting up Persistent resources ({:?}) for {} (region={}, channel={}, sub-channel={})",
        file_set,
        game_id,
        region_id,
        channel_id.channel(),
        channel_id.sub_channel()
    ));
    ui::print_info(format!(
        "StreamingAssets source: {}",
        streaming_assets_root.display()
    ));
    ui::print_info(format!("Persistent target: {}", persistent_root.display()));

    let pool_cfg = opts.task_pool_config();
    let mut task_pool_runner = TaskPoolRunner::new(pool_cfg)?;

    let progress = CountAndByteProgress::new(
        "resources.sync.verify",
        "resources.sync.download",
        opts.verbose,
    );
    let progress_session = progress.start(ProgressLane::VFS_VERIFY, ProgressLane::VFS_DOWNLOAD);
    let result = setup_persistent_vfs(
        &api_client,
        &install_target.api,
        &version_info.version,
        &rand_str,
        &persistent_root,
        &PersistentVfsConfig {
            file_set,
            source_streaming_assets: streaming_assets_root,
            extra_source_streaming_assets,
            prefer_reuse,
            allow_download,
            prune_extra_files,
            state_root: griffr_common::runtime::griffr_path(&local.install_path),
        },
        &mut task_pool_runner,
        progress_session.sender(),
    )
    .await
    .context("Failed to set up Persistent resources")?;
    progress_session.finish();
    progress.finish();

    if let Some(result) = result {
        ui::print_info(format!(
            "File set: {} | res_version={}",
            result.file_set, result.res_version
        ));
        ui::print_info(format!(
            "Persistent resources: total={} reused={} downloaded={} ({}) skipped={} failed={}",
            result.total_files,
            result.reused_files,
            result.downloaded_files,
            ui::format_bytes(result.downloaded_bytes),
            result.skipped_files,
            result.failed_files
        ));
        if result.failed_files > 0 {
            anyhow::bail!(
                "Persistent resources setup failed for {} file(s)",
                result.failed_files
            );
        }
        ui::print_success("Persistent resources setup finished");
    } else {
        ui::print_info(
            "Persistent resources setup was skipped because this target does not support VFS.",
        );
    }

    Ok(())
}
