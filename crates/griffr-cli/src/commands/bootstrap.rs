use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::game::task_pool::{TaskPoolConfig, TaskPoolRunner};
use griffr_common::game::{
    bootstrap_persistent_vfs_with_runner, VfsBootstrapConfig, VfsBootstrapScope,
};

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::ui;
use crate::{BootstrapScope, GlobalOptions};

fn map_scope(scope: BootstrapScope) -> VfsBootstrapScope {
    match scope {
        BootstrapScope::Initial => VfsBootstrapScope::Initial,
        BootstrapScope::Complete => VfsBootstrapScope::Complete,
    }
}

pub async fn bootstrap(
    path: PathBuf,
    scope: BootstrapScope,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    allow_download: bool,
    relink_reuse: bool,
    prune_extra_files: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let server_id = local.require_known_server()?;
    let installed_version = local.require_config_ini_version()?.to_string();
    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(game_id, server_id, Some(&installed_version))
        .await
        .context("Failed to fetch version information for bootstrap")?;

    let rand_str = version_info.rand_str();
    if rand_str.is_empty() {
        anyhow::bail!(
            "Could not resolve rand_str for {} ({}) version {}",
            game_id,
            server_id,
            installed_version
        );
    }

    let data_root = local.install_path.join(game_id.streaming_assets_subdir());
    let streaming_assets_root = data_root.join("StreamingAssets");
    let persistent_root = data_root.join("Persistent");

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
                game_id
            );
        }
        if source.install_path != local.install_path {
            extra_source_streaming_assets.push(
                source
                    .install_path
                    .join(game_id.streaming_assets_subdir())
                    .join("StreamingAssets"),
            );
        }
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would bootstrap Persistent VFS for {} ({}) at {} with scope={:?}",
            game_id,
            server_id,
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
        "Bootstrapping Persistent VFS ({:?}) for {} ({})",
        scope, game_id, server_id
    ));
    ui::print_info(format!(
        "StreamingAssets source: {}",
        streaming_assets_root.display()
    ));
    ui::print_info(format!("Persistent target: {}", persistent_root.display()));

    let mut pool_cfg = TaskPoolConfig::default();
    pool_cfg.max_retries = 3;
    let mut task_pool_runner = TaskPoolRunner::new(pool_cfg)?;

    let progress = Arc::new(StepProgress::new("bootstrap.persistent-vfs", opts.verbose));
    let progress_cb = progress.clone();
    let result = bootstrap_persistent_vfs_with_runner(
        &api_client,
        game_id,
        server_id,
        &version_info.version,
        &rand_str,
        &persistent_root,
        &VfsBootstrapConfig {
            scope: map_scope(scope),
            source_streaming_assets: streaming_assets_root,
            extra_source_streaming_assets,
            allow_copy_fallback: force_copy,
            prefer_reuse: relink_reuse,
            allow_download,
            prune_extra_files,
        },
        &mut task_pool_runner,
        Some(&move |current, total| {
            progress_cb.update(current as usize, total as usize, "persistent-vfs");
        }),
    )
    .await
    .context("Failed to bootstrap Persistent VFS")?;
    progress.finish();

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

    Ok(())
}
