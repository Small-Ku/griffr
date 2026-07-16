use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::GetLatestGameResponse;
use griffr_common::runtime::task_pool::TaskPoolRunner;
use griffr_common::runtime::{
    ensure_game_files_with_pool, resolve_file_reuse_sources, FileReuseConfig, LocalInstall,
    ProgressLane,
};

use crate::progress::CountAndByteProgress;
use crate::ui;
use crate::GlobalOptions;

pub(super) async fn update_via_reuse(
    api_client: &ApiClient,
    local: &LocalInstall,
    version_info: &GetLatestGameResponse,
    reuse_paths: &[PathBuf],
    force_copy: bool,
    opts: &GlobalOptions,
    task_pool_runner: &mut TaskPoolRunner,
) -> Result<()> {
    let game_id = local.require_known_game()?;
    let pkg = version_info
        .pkg
        .as_ref()
        .context("No full package information available for reuse update")?;
    let source_installs =
        resolve_file_reuse_sources(&game_id, &local.install_path, reuse_paths).await?;

    opts.verbose(format!(
        "Applying file reuse from {} compatible source install(s)",
        source_installs.len()
    ));

    let ensure_progress = CountAndByteProgress::new(
        "update.ensure_files",
        "update.ensure_files.download",
        opts.verbose,
    );
    let ensure_session = ensure_progress.start(
        ProgressLane::FILE_ENSURE_VERIFY,
        ProgressLane::FILE_ENSURE_DOWNLOAD,
    );
    let ensured = ensure_game_files_with_pool(
        api_client,
        game_id,
        &local.install_path,
        &pkg.file_path,
        pkg.game_files_md5.as_deref(),
        &FileReuseConfig {
            allow_copy_fallback: force_copy,
            dry_run: opts.is_dry_run(),
            source_installs,
        },
        Some(task_pool_runner),
        ensure_session.sender(),
    )
    .await?;
    ensure_session.finish();
    ensure_progress.finish();

    ui::print_info(format!(
        "Ensured files: reused={} downloaded={}",
        ensured.reused_files, ensured.downloaded_files
    ));
    if !ensured.issues.is_empty() {
        anyhow::bail!(
            "Update file ensure operation finished with {} issue(s)",
            ensured.issues.len()
        );
    }
    Ok(())
}
