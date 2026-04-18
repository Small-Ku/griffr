use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::GameId;
use griffr_common::game::task_pool::{
    run_tasks_with_progress, ArchivePart, ProgressEvent, Task, TaskPoolConfig,
};
use griffr_common::game::{
    download_vfs_resources, materialize_game_files_with_pool, FileReuseConfig, GameManager,
    SourceInstallInput,
};
use tracing::{info, warn};

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::GlobalOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdatePackageKind {
    Patch,
    Full,
}

fn strip_url_query(s: &str) -> &str {
    s.split('?').next().unwrap_or(s)
}

fn archive_base_from_url(url: &str) -> Option<String> {
    let filename = url.split('/').next_back()?;
    let filename = strip_url_query(filename);

    if let Some(stem) = filename.strip_suffix(".zip.001") {
        return Some(stem.to_string());
    }

    if let Some(stem) = filename.strip_suffix(".zip") {
        return Some(stem.to_string());
    }

    None
}

fn is_launcher_metadata_issue(path: &str) -> bool {
    matches!(
        path.replace('\\', "/").to_ascii_lowercase().as_str(),
        "game_files" | "package_files"
    )
}

fn choose_update_package(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    current_version: Option<&str>,
) -> Result<UpdatePackageKind> {
    let patch_matches_installed_version = current_version
        .zip(Some(version_info.request_version.as_str()))
        .is_some_and(|(current, requested)| !current.is_empty() && current == requested);

    if version_info.has_patch_package() && patch_matches_installed_version {
        return Ok(UpdatePackageKind::Patch);
    }

    if version_info.has_full_package() {
        return Ok(UpdatePackageKind::Full);
    }

    if version_info.has_patch_package() {
        anyhow::bail!(
            "Patch package was returned for request version '{}' but the installed version is {:?}",
            version_info.request_version,
            current_version
        );
    }

    anyhow::bail!(
        "Update is available but the API returned neither patch nor full package archives"
    )
}

async fn verify_updated_install(
    api_client: &ApiClient,
    manager: &mut GameManager,
    target_version: &str,
    skip_verify: bool,
) -> Result<()> {
    if skip_verify {
        manager.set_version(target_version.to_string());
        manager
            .sync_launcher_metadata(api_client)
            .await
            .context("Failed to sync launcher metadata after update")?;
        return Ok(());
    }

    let summary = manager
        .run_integrity_pool(api_client, true, &[], false, None::<fn(usize, usize, &str)>)
        .await?;
    info!("update.verify.issues={}", summary.issues.len());
    info!(
        "update.repair.downloaded_files={}",
        summary.downloaded_files
    );
    for issue in summary.issues.iter().take(20) {
        warn!("{} {:?}", issue.path, issue.kind);
    }
    let remaining_non_metadata = summary
        .issues
        .iter()
        .filter(|issue| !is_launcher_metadata_issue(&issue.path))
        .count();
    if remaining_non_metadata > 0 {
        anyhow::bail!(
            "Post-update integrity has {} non-metadata issue(s)",
            remaining_non_metadata
        );
    }

    manager.set_version(target_version.to_string());
    manager
        .sync_launcher_metadata(api_client)
        .await
        .context("Failed to sync launcher metadata after update")?;
    Ok(())
}

async fn update_via_reuse(
    api_client: &ApiClient,
    local: &super::local::LocalInstall,
    manager: &mut GameManager,
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    reuse_paths: &[PathBuf],
    force_copy: bool,
    opts: &GlobalOptions,
) -> Result<()> {
    let game_id = local.require_known_game()?;
    let target_server_id = local.require_known_server()?;

    let pkg = version_info
        .pkg
        .as_ref()
        .context("No full package information available for reuse update")?;

    let mut source_installs = Vec::new();

    for reuse_path in reuse_paths {
        let source = detect_local_install(reuse_path)
            .await
            .with_context(|| format!("Failed to inspect reuse source {}", reuse_path.display()))?;
        let source_game_id = source.require_known_game()?;
        if source_game_id != game_id {
            anyhow::bail!(
                "Reuse source {} is {:?}, expected {:?}",
                source.install_path.display(),
                source_game_id,
                game_id
            );
        }
        let source_server_id = source.require_known_server()?;
        let source_version = source.require_version()?.to_string();
        if source.install_path == local.install_path {
            continue;
        }
        source_installs.push(SourceInstallInput {
            server_id: source_server_id,
            version: source_version,
            install_path: source.install_path.clone(),
        });
    }

    opts.verbose(format!(
        "Applying file reuse from {} source install(s)",
        reuse_paths.len()
    ));

    let materialized = materialize_game_files_with_pool(
        api_client,
        game_id,
        target_server_id,
        &version_info.version,
        &local.install_path,
        &pkg.file_path,
        pkg.game_files_md5.as_deref(),
        &FileReuseConfig {
            allow_copy_fallback: force_copy,
            dry_run: opts.is_dry_run(),
            source_installs,
        },
        Some(|current: usize, total: usize, file: &str| {
            if opts.verbose || total <= 10 || current % 25 == 0 {
                info!("update.materialize {}/{} {}", current + 1, total, file);
            }
        }),
    )
    .await?;

    info!("update.reused_files={}", materialized.reused_files);
    info!("update.downloaded_files={}", materialized.downloaded_files);
    if !materialized.issues.is_empty() {
        anyhow::bail!(
            "Update materialization finished with {} issue(s)",
            materialized.issues.len()
        );
    }
    verify_updated_install(api_client, manager, &version_info.version, opts.skip_verify).await?;
    Ok(())
}

async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    opts: &GlobalOptions,
) -> Result<()> {
    let total_size: u64 = archives.iter().map(|p| p.size()).sum();
    info!("update.label={} bytes={}", label, total_size);

    let download_dir = install_path.join("downloads");
    compio::fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("Failed to create {}", download_dir.display()))?;

    let mut grouped: HashMap<String, Vec<ArchivePart>> = HashMap::new();
    for archive in archives {
        let filename = archive
            .filename()
            .context("Failed to extract archive filename")?
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let base = archive_base_from_url(&archive.url)
            .context("Could not determine archive base name from pack URL")?;
        grouped.entry(base).or_default().push(ArchivePart {
            url: archive.url.clone(),
            dest: download_dir.join(&filename),
            logical_path: filename,
            expected_md5: archive.md5.clone(),
            expected_size: archive.size(),
        });
    }
    if grouped.is_empty() {
        anyhow::bail!("No archives to process");
    }

    let mut tasks = Vec::with_capacity(grouped.len());
    for (base_name, mut parts) in grouped {
        parts.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));
        opts.verbose(format!("queued archive state-machine {}", base_name));
        tasks.push(Task::InstallArchive {
            source_dir: download_dir.clone(),
            base_name,
            dest: install_path.to_path_buf(),
            cleanup: true,
            parts,
        });
    }

    let archive_total = tasks.len();
    let archive_bar = Arc::new(StepProgress::new(
        format!("update.{}.archives", label),
        opts.verbose,
    ));
    let archive_bar_cb = archive_bar.clone();
    let mut archive_done = 0usize;
    let mut cfg = TaskPoolConfig::default();
    cfg.io_slots = 4;
    cfg.max_retries = 3;
    let result = run_tasks_with_progress(
        tasks,
        cfg,
        Some(&mut |event: &ProgressEvent| {
            if let ProgressEvent::Extracted { path } = event {
                archive_bar_cb.update(
                    archive_done,
                    archive_total,
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("archive"),
                );
                archive_done += 1;
            }
        }),
    )?;
    archive_bar.finish();

    let mut failures = Vec::new();
    for event in result.events {
        if let ProgressEvent::Failed { path, reason } = event {
            failures.push(format!("{} ({})", path, reason));
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Update archive pipeline failed for {} item(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    Ok(())
}

async fn validate_patch_target(game_id: GameId, install_path: &Path) -> Result<()> {
    let expected_exe = install_path.join(match game_id {
        GameId::Arknights => "Arknights.exe",
        GameId::Endfield => "Endfield.exe",
    });
    match compio::fs::metadata(&expected_exe).await {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            anyhow::bail!("Patch target missing {}", expected_exe.display());
        }
        Err(err) => Err(err)
            .with_context(|| format!("Failed to stat patch target {}", expected_exe.display())),
    }
}

pub async fn update(
    path: PathBuf,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let local = detect_local_install(&path).await?;
    let game_id = local.require_known_game()?;
    let server_id = local.require_known_server()?;
    let current_version = local.require_version()?.to_string();
    let mut manager = local.as_manager()?;
    let api_client = ApiClient::new()?;

    let version_info = api_client
        .get_latest_game(game_id, server_id, Some(&current_version))
        .await?;

    info!(
        "update path={} game={:?} server={} current={} latest={}",
        local.install_path.display(),
        game_id,
        server_id,
        current_version,
        version_info.version
    );

    if current_version == version_info.version || !version_info.has_update() {
        info!("update noop");
        return Ok(());
    }

    let package_kind = if opts.force_full_package {
        UpdatePackageKind::Full
    } else {
        choose_update_package(&version_info, Some(&current_version))?
    };

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would update {} from {} to {} using {:?}",
            local.install_path.display(),
            current_version,
            version_info.version,
            package_kind
        ));
        if !reuse_paths.is_empty() {
            opts.dry_run(format!(
                "Would reuse files from: {}",
                reuse_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Ok(());
    }

    if !reuse_paths.is_empty() {
        update_via_reuse(
            &api_client,
            &local,
            &mut manager,
            &version_info,
            &reuse_paths,
            force_copy,
            &opts,
        )
        .await?;

        if !opts.skip_vfs {
            let streaming_assets = local
                .install_path
                .join(game_id.streaming_assets_subdir())
                .join("StreamingAssets");
            let rand_str = version_info.rand_str();
            let _ = download_vfs_resources(
                &api_client,
                game_id,
                server_id,
                &version_info.version,
                &rand_str,
                &streaming_assets,
                None,
            )
            .await;
        }

        info!("update complete");
        return Ok(());
    }

    match package_kind {
        UpdatePackageKind::Patch => {
            validate_patch_target(game_id, &local.install_path).await?;
            let patch = version_info
                .patch
                .as_ref()
                .context("No patch package information available")?;
            download_and_extract_archives(&patch.patches, &local.install_path, "patch", &opts)
                .await?;
            verify_updated_install(
                &api_client,
                &mut manager,
                &version_info.version,
                opts.skip_verify,
            )
            .await?;
        }
        UpdatePackageKind::Full => {
            let pkg = version_info
                .pkg
                .as_ref()
                .context("No full package information available")?;
            download_and_extract_archives(&pkg.packs, &local.install_path, "full", &opts).await?;
            verify_updated_install(
                &api_client,
                &mut manager,
                &version_info.version,
                opts.skip_verify,
            )
            .await?;
        }
    }

    if !opts.skip_vfs {
        let streaming_assets = local
            .install_path
            .join(game_id.streaming_assets_subdir())
            .join("StreamingAssets");
        let rand_str = version_info.rand_str();
        let _ = download_vfs_resources(
            &api_client,
            game_id,
            server_id,
            &version_info.version,
            &rand_str,
            &streaming_assets,
            None,
        )
        .await;
    }

    info!("update complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use griffr_common::api::types::{GetLatestGameResponse, PackFile, PackageInfo, PatchInfo};

    #[test]
    fn archive_base_name_is_extracted() {
        let url = "https://example.com/path/Beyond_Release_v1d1.zip.001?token=abc";
        assert_eq!(
            archive_base_from_url(url).as_deref(),
            Some("Beyond_Release_v1d1")
        );
    }

    #[test]
    fn archive_base_name_supports_single_zip_archives() {
        let url = "https://example.com/path/Beyond_Release_v1d1.zip?token=abc";
        assert_eq!(
            archive_base_from_url(url).as_deref(),
            Some("Beyond_Release_v1d1")
        );
    }

    #[test]
    fn choose_patch_package_when_available() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: Some("def".to_string()),
            }),
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![PackFile {
                    url: "https://example.com/patch.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                package_size: "1".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        assert_eq!(
            choose_update_package(&response, Some("1.0.13")).unwrap(),
            UpdatePackageKind::Patch
        );
    }

    #[test]
    fn choose_full_package_when_patch_missing() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: Some("def".to_string()),
            }),
            patch: None,
            state: 0,
            launcher_action: 0,
        };

        assert_eq!(
            choose_update_package(&response, Some("1.0.13")).unwrap(),
            UpdatePackageKind::Full
        );
    }

    #[test]
    fn choose_full_package_when_patch_version_mismatches() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: Some("def".to_string()),
            }),
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![PackFile {
                    url: "https://example.com/patch.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                package_size: "1".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        assert_eq!(
            choose_update_package(&response, Some("1.0.14")).unwrap(),
            UpdatePackageKind::Full
        );
    }

    #[test]
    fn reject_patch_only_update_when_version_mismatches() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: None,
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![PackFile {
                    url: "https://example.com/patch.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "1".to_string(),
                }],
                total_size: "1".to_string(),
                package_size: "1".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        let err = choose_update_package(&response, Some("1.0.14")).unwrap_err();
        assert!(err.to_string().contains("Patch package was returned"));
    }
}
