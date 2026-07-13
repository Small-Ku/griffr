use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::runtime::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::runtime::{
    bootstrap_persistent_vfs_with_runner, VfsBootstrapConfig, VfsBootstrapScope,
};

use super::local::detect_local_install;
use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;

pub async fn bootstrap(
    path: PathBuf,
    overrides: crate::InstallTargetOverrideArgs,
    scope: VfsBootstrapScope,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    allow_download: bool,
    relink_reuse: bool,
    prune_extra_files: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
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
        .context("Failed to fetch version information for bootstrap")?;

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

    let mut extra_source_streaming_assets = Vec::new();
    for reuse in &reuse_paths {
        let source = detect_local_install(reuse)
            .await
            .with_context(|| format!("Failed to inspect reuse source {}", reuse.display()))?;
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
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would bootstrap Persistent VFS for {} (region={}, channel={}, sub-channel={}) at {} with scope={:?}",
            game_id,
            region_id,
            channel_id.channel(),
            channel_id.sub_channel(),
            local.install_path.display(),
            scope
        ));
        opts.dry_run(format!(
            "Would use source StreamingAssets: {}",
            streaming_assets_root.display()
        ));
        if !extra_source_streaming_assets.is_empty() {
            opts.dry_run(format!(
                "Would use additional reuse StreamingAssets roots: {}",
                extra_source_streaming_assets
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        opts.dry_run(format!(
            "Would materialize into Persistent root: {}",
            persistent_root.display()
        ));
        opts.dry_run(format!(
            "allow_download={} relink_reuse={} force_copy={} prune_extra_files={}",
            allow_download, relink_reuse, force_copy, prune_extra_files
        ));
        return Ok(());
    }

    ui::print_phase(format!(
        "Bootstrapping Persistent VFS ({:?}) for {} (region={}, channel={}, sub-channel={})",
        scope,
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

    let pool_cfg = TaskPoolConfig::with_progress_buffers(
        opts.extraction_progress_buffer_bytes,
        opts.download_progress_buffer_bytes,
    );
    let mut task_pool_runner = TaskPoolRunner::new(pool_cfg)?;

    let progress = CountAndByteProgress::new(
        "bootstrap.persistent-vfs",
        "bootstrap.persistent-vfs.download",
        opts.verbose,
    );
    let (progress_cb, download_progress_cb) = progress.split_callbacks();
    let result = bootstrap_persistent_vfs_with_runner(
        &api_client,
        &install_target.api,
        &version_info.version,
        &rand_str,
        &persistent_root,
        &VfsBootstrapConfig {
            scope,
            source_streaming_assets: streaming_assets_root,
            extra_source_streaming_assets,
            allow_copy_fallback: force_copy,
            prefer_reuse: relink_reuse,
            allow_download,
            prune_extra_files,
        },
        &mut task_pool_runner,
        Some(&progress_cb),
        Some(&download_progress_cb),
    )
    .await
    .context("Failed to bootstrap Persistent VFS")?;
    progress.finish();

    if let Some(result) = result {
        ui::print_info(format!(
            "Bootstrap scope: {} | res_version={}",
            result.scope_label, result.res_version
        ));
        ui::print_info(format!(
            "Persistent VFS: total={} reused={} downloaded={} ({}) skipped={} failed={}",
            result.total_files,
            result.reused_files,
            result.downloaded_files,
            ui::format_bytes(result.downloaded_bytes),
            result.skipped_files,
            result.failed_files
        ));
        if result.failed_files > 0 {
            anyhow::bail!(
                "Persistent bootstrap finished with {} failed file(s)",
                result.failed_files
            );
        }
        ui::print_success("Persistent bootstrap complete");
    } else {
        ui::print_info("Persistent VFS bootstrap skipped (VFS not supported for this target).");
    }

    Ok(())
}
