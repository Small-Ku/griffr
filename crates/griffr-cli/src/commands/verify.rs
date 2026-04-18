use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use tracing::{info, warn};

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::GlobalOptions;

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
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let server_id = local.require_known_server()?;
    let installed_version = local.require_version()?.to_string();
    let manager = local.as_manager()?;
    let api_client = ApiClient::new()?;

    info!(
        "verify path={} game={:?} server={} version={}",
        local.install_path.display(),
        game_id,
        server_id,
        installed_version
    );

    let verify_bar = Arc::new(StepProgress::new(
        if repair { "verify+repair" } else { "verify" },
        opts.verbose,
    ));
    let verify_bar_cb = verify_bar.clone();
    let progress_cb = |current: usize, total: usize, file: &str| {
        verify_bar_cb.update(current, total, file);
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

    let summary = manager
        .run_integrity_pool(
            &api_client,
            repair,
            &source_roots,
            force_copy,
            Some(progress_cb),
        )
        .await
        .context("run_integrity_pool failed")?;
    verify_bar.finish();
    info!("verify issues={}", summary.issues.len());
    if repair {
        info!("repair.downloaded_files={}", summary.downloaded_files);
        info!("repair.reused_files={}", summary.reused_files);
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
        warn!(
            "{} {:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
            issue.path,
            issue.kind,
            issue.expected_size,
            issue.actual_size,
            issue.expected_md5,
            issue.actual_md5
        );
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
            info!(
                "repair.metadata_issues_ignored={} (metadata will be normalized by launcher sync)",
                metadata_issues.len()
            );
        }
        info!("repair.syncing_metadata=started");
        manager
            .sync_launcher_metadata(&api_client)
            .await
            .context("Failed to sync launcher metadata after repair")?;
        info!("repair.syncing_metadata=done");

        if remaining_non_metadata > 0 {
            anyhow::bail!(
                "verify+repair finished with {} remaining non-metadata issue(s)",
                remaining_non_metadata
            );
        }
    }

    Ok(())
}
