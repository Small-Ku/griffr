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

use super::local::detect_local_install;
use crate::progress::StepProgress;
use crate::ui;
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

    if let Some(idx) = filename.rfind(".zip.") {
        let suffix = &filename[(idx + ".zip.".len())..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return Some(filename[..idx].to_string());
        }
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

fn describe_update_package_selection(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    current_version: Option<&str>,
    package_kind: UpdatePackageKind,
    force_full_package: bool,
) -> String {
    if force_full_package {
        return "Using full package (--full-package set; patch selection bypassed).".to_string();
    }

    match package_kind {
        UpdatePackageKind::Patch => {
            let installed = current_version.unwrap_or("<unknown>");
            format!(
                "Using patch package: installed version '{}' matches request_version '{}'.",
                installed, version_info.request_version
            )
        }
        UpdatePackageKind::Full => {
            if version_info.patch.is_some() {
                let installed = current_version.unwrap_or("<unknown>");
                format!(
                    "Using full package: patch request_version '{}' does not match installed version '{}'.",
                    version_info.request_version, installed
                )
            } else {
                "Using full package: API did not provide a compatible patch package.".to_string()
            }
        }
    }
}

fn selected_archive_plan(
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    package_kind: UpdatePackageKind,
) -> Option<(&'static str, usize, u64)> {
    match package_kind {
        UpdatePackageKind::Patch => version_info.patch.as_ref().map(|patch| {
            let count = patch.patches.len();
            let total_size = patch.patches.iter().map(|p| p.size()).sum();
            ("patch", count, total_size)
        }),
        UpdatePackageKind::Full => version_info.pkg.as_ref().map(|pkg| {
            let count = pkg.packs.len();
            let total_size = pkg.packs.iter().map(|p| p.size()).sum();
            ("full", count, total_size)
        }),
    }
}

fn build_update_dry_run_plan(
    install_path: &Path,
    current_version: &str,
    version_info: &griffr_common::api::types::GetLatestGameResponse,
    package_kind: UpdatePackageKind,
    reuse_paths: &[PathBuf],
    skip_verify: bool,
    skip_vfs: bool,
    force_full_package: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "Would update {} from {} to {} using {:?}",
        install_path.display(),
        current_version,
        version_info.version,
        package_kind
    ));
    lines.push(describe_update_package_selection(
        version_info,
        Some(current_version),
        package_kind,
        force_full_package,
    ));

    if !reuse_paths.is_empty() {
        lines.push(format!(
            "Would apply update via local file reuse from: {}",
            reuse_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if let Some((label, archive_count, total_size)) =
        selected_archive_plan(version_info, package_kind)
    {
        lines.push(format!(
            "Would download {label} archive parts: {archive_count} ({})",
            ui::format_bytes(total_size)
        ));
    } else {
        lines.push("Would download update archives based on API response.".to_string());
    }

    if skip_verify {
        lines.push("Would skip post-update integrity verification (--skip-verify).".to_string());
    } else {
        lines.push("Would run post-update integrity verification.".to_string());
    }

    if skip_vfs {
        lines.push("Would skip VFS resource sync (--skip-vfs).".to_string());
    } else {
        lines.push("Would sync VFS resources after update.".to_string());
    }

    lines
}

async fn verify_updated_install(
    api_client: &ApiClient,
    manager: &mut GameManager,
    target_version: &str,
    install_path: &Path,
    skip_verify: bool,
) -> Result<()> {
    if skip_verify {
        ui::print_info("Skipping post-update integrity verification (--skip-verify)");
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
    ui::print_info(format!(
        "Verification summary: issues={} repaired_downloads={}",
        summary.issues.len(),
        summary.downloaded_files
    ));
    for issue in summary.issues.iter().take(20) {
        ui::print_warning(format!("{} {:?}", issue.path, issue.kind));
    }
    let remaining_non_metadata = summary
        .issues
        .iter()
        .filter(|issue| !is_launcher_metadata_issue(&issue.path))
        .count();
    if remaining_non_metadata > 0 {
        anyhow::bail!(
            "Post-update integrity has {} non-metadata issue(s). Re-run `griffr verify --path \"{}\" --repair` and then `griffr update --path \"{}\"`.",
            remaining_non_metadata,
            install_path.display(),
            install_path.display()
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
        let source_version = source.require_config_ini_version()?.to_string();
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

    let materialize_bar = Arc::new(StepProgress::new("update.materialize", opts.verbose));
    let materialize_bar_cb = materialize_bar.clone();
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
            materialize_bar_cb.update(current, total, file);
        }),
    )
    .await?;
    materialize_bar.finish();

    ui::print_info(format!(
        "Materialized files: reused={} downloaded={}",
        materialized.reused_files, materialized.downloaded_files
    ));
    if !materialized.issues.is_empty() {
        anyhow::bail!(
            "Update materialization finished with {} issue(s)",
            materialized.issues.len()
        );
    }
    verify_updated_install(
        api_client,
        manager,
        &version_info.version,
        &local.install_path,
        opts.skip_verify,
    )
    .await?;
    Ok(())
}

async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    opts: &GlobalOptions,
) -> Result<()> {
    let total_size: u64 = archives.iter().map(|p| p.size()).sum();
    ui::print_phase(format!(
        "Downloading {label} package archives ({})",
        ui::format_bytes(total_size)
    ));

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
    let current_version = local.require_config_ini_version()?.to_string();
    let mut manager = local.as_manager()?;
    let api_client = ApiClient::new()?;

    let version_info = api_client
        .get_latest_game(game_id, server_id, Some(&current_version))
        .await?;

    ui::print_phase(format!(
        "Updating {} ({}) at {}",
        game_id,
        server_id,
        local.install_path.display(),
    ));
    ui::print_info(format!(
        "Current version (config.ini): {} | Latest version: {}",
        current_version, version_info.version
    ));
    if opts.verbose {
        ui::print_info(format!(
            "Update API versions: request_version='{}' response.request_version='{}' target_version='{}'",
            current_version, version_info.request_version, version_info.version
        ));
    }

    if current_version == version_info.version || !version_info.has_update() {
        ui::print_success("Already up to date");
        return Ok(());
    }

    let package_kind = if opts.force_full_package {
        UpdatePackageKind::Full
    } else {
        choose_update_package(&version_info, Some(&current_version))?
    };
    ui::print_info(describe_update_package_selection(
        &version_info,
        Some(&current_version),
        package_kind,
        opts.force_full_package,
    ));

    if opts.is_dry_run() {
        for line in build_update_dry_run_plan(
            &local.install_path,
            &current_version,
            &version_info,
            package_kind,
            &reuse_paths,
            opts.skip_verify,
            opts.skip_vfs,
            opts.force_full_package,
        ) {
            opts.dry_run(line);
        }
        return Ok(());
    }

    if !reuse_paths.is_empty() {
        ui::print_phase("Applying update via local file reuse");
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
            ui::print_phase("Syncing VFS resources");
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

        ui::print_success("Update complete");
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
                &local.install_path,
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
                &local.install_path,
                opts.skip_verify,
            )
            .await?;
        }
    }

    if !opts.skip_vfs {
        ui::print_phase("Syncing VFS resources");
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

    ui::print_success("Update complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use griffr_common::api::client::ApiClient;
    use griffr_common::api::types::{GetLatestGameResponse, PackFile, PackageInfo, PatchInfo};
    use griffr_common::config::{GameId, ServerId};
    use md5::Digest;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;
    use zip::write::FileOptions;

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
    fn archive_base_name_is_extracted_from_non_first_split_part() {
        let url = "https://example.com/path/Beyond_Release_v1d1.zip.002?token=abc";
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

    #[test]
    fn describe_selection_mentions_patch_match_reason() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![],
                total_size: "0".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: None,
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
                total_size: "0".to_string(),
                package_size: "0".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        let msg = describe_update_package_selection(
            &response,
            Some("1.0.13"),
            UpdatePackageKind::Patch,
            false,
        );
        assert!(msg.contains("Using patch package"));
        assert!(msg.contains("matches request_version"));
    }

    #[test]
    fn describe_selection_mentions_full_fallback_when_patch_mismatch() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![],
                total_size: "0".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: None,
            }),
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![],
                total_size: "0".to_string(),
                package_size: "0".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        let msg = describe_update_package_selection(
            &response,
            Some("1.0.14"),
            UpdatePackageKind::Full,
            false,
        );
        assert!(msg.contains("Using full package"));
        assert!(msg.contains("does not match"));
    }

    #[test]
    fn describe_selection_mentions_forced_full() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![],
                total_size: "0".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: None,
            }),
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![],
                total_size: "0".to_string(),
                package_size: "0".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        let msg = describe_update_package_selection(
            &response,
            Some("1.0.13"),
            UpdatePackageKind::Full,
            true,
        );
        assert!(msg.contains("--full-package"));
    }

    #[test]
    fn selected_archive_plan_uses_patch_parts() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: None,
            patch: Some(PatchInfo {
                url: "https://example.com/patch.zip".to_string(),
                md5: "abc".to_string(),
                file_id: "1".to_string(),
                patches: vec![
                    PackFile {
                        url: "https://example.com/patch.zip.001".to_string(),
                        md5: "abc".to_string(),
                        package_size: "3".to_string(),
                    },
                    PackFile {
                        url: "https://example.com/patch.zip.002".to_string(),
                        md5: "def".to_string(),
                        package_size: "4".to_string(),
                    },
                ],
                total_size: "7".to_string(),
                package_size: "7".to_string(),
            }),
            state: 0,
            launcher_action: 0,
        };

        let plan = selected_archive_plan(&response, UpdatePackageKind::Patch).unwrap();
        assert_eq!(plan.0, "patch");
        assert_eq!(plan.1, 2);
        assert_eq!(plan.2, 7);
    }

    #[test]
    fn dry_run_plan_includes_verify_and_vfs_steps() {
        let response = GetLatestGameResponse {
            action: 1,
            request_version: "1.0.13".to_string(),
            version: "1.1.9".to_string(),
            pkg: Some(PackageInfo {
                packs: vec![PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "8".to_string(),
                }],
                total_size: "8".to_string(),
                file_path: "https://example.com/files".to_string(),
                game_files_md5: Some("def".to_string()),
            }),
            patch: None,
            state: 0,
            launcher_action: 0,
        };

        let lines = build_update_dry_run_plan(
            Path::new("C:\\Games\\Endfield"),
            "1.0.13",
            &response,
            UpdatePackageKind::Full,
            &[],
            false,
            false,
            false,
        );

        assert!(lines
            .iter()
            .any(|line| line.contains("Would download full archive parts")));
        assert!(lines
            .iter()
            .any(|line| line.contains("Would run post-update integrity verification")));
        assert!(lines
            .iter()
            .any(|line| line.contains("Would sync VFS resources after update")));
    }

    fn start_test_http_server(
        routes: HashMap<String, Vec<u8>>,
    ) -> (String, Arc<Mutex<HashMap<String, usize>>>, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking test server");
        let addr = listener.local_addr().expect("server addr");
        let hits = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_thread = Arc::clone(&hits);
        let stop_thread = Arc::clone(&stop);

        thread::spawn(move || {
            while !stop_thread.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 4096];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        if read == 0 {
                            continue;
                        }
                        let req = String::from_utf8_lossy(&buf[..read]);
                        let first_line = req.lines().next().unwrap_or_default();
                        let path = first_line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .to_string();

                        {
                            let mut guard = hits_thread.lock().unwrap();
                            *guard.entry(path.clone()).or_insert(0) += 1;
                        }

                        if let Some(body) = routes.get(&path) {
                            let header = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                        } else {
                            let body = b"not found";
                            let header = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(header.as_bytes());
                            let _ = stream.write_all(body);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        (format!("http://{}", addr), hits, stop)
    }

    #[compio::test]
    async fn download_and_extract_archives_recovers_partial_part_on_rerun() {
        let tmp = tempdir().unwrap();
        let install_path = tmp.path().join("install");
        let download_dir = install_path.join("downloads");
        std::fs::create_dir_all(&download_dir).unwrap();
        std::fs::create_dir_all(&install_path).unwrap();

        let zip_path = tmp.path().join("bundle.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("payload.txt", FileOptions::<()>::default())
            .unwrap();
        zip.write_all(b"cli updater recovery").unwrap();
        zip.finish().unwrap();
        let zip_bytes = std::fs::read(&zip_path).unwrap();

        let split_at = (zip_bytes.len() / 2).max(1);
        let part1 = zip_bytes[..split_at].to_vec();
        let part2 = zip_bytes[split_at..].to_vec();
        assert!(!part2.is_empty());

        // Simulate interrupted prior run:
        // - part1 fully downloaded and valid
        // - part2 truncated/corrupted
        let part1_name = "bundle.zip.001";
        let part2_name = "bundle.zip.002";
        std::fs::write(download_dir.join(part1_name), &part1).unwrap();
        std::fs::write(
            download_dir.join(part2_name),
            &part2[..(part2.len() / 2).max(1)],
        )
        .unwrap();

        let mut routes = HashMap::new();
        routes.insert(format!("/{}", part1_name), part1.clone());
        routes.insert(format!("/{}", part2_name), part2.clone());
        let (base_url, hits, stop) = start_test_http_server(routes);

        let archives = vec![
            PackFile {
                url: format!("{}/{}", base_url, part1_name),
                md5: format!("{:x}", md5::Md5::digest(&part1)),
                package_size: part1.len().to_string(),
            },
            PackFile {
                url: format!("{}/{}", base_url, part2_name),
                md5: format!("{:x}", md5::Md5::digest(&part2)),
                package_size: part2.len().to_string(),
            },
        ];

        let opts = GlobalOptions {
            dry_run: false,
            verbose: false,
            skip_verify: false,
            force_full_package: false,
            skip_vfs: true,
            output: crate::OutputFormat::Text,
        };

        let result = download_and_extract_archives(&archives, &install_path, "patch", &opts).await;
        stop.store(true, Ordering::Release);
        result.unwrap();

        let guard = hits.lock().unwrap();
        assert_eq!(
            guard.get(&format!("/{}", part1_name)).copied().unwrap_or(0),
            0,
            "valid part should be reused and skipped"
        );
        assert_eq!(
            guard.get(&format!("/{}", part2_name)).copied().unwrap_or(0),
            1,
            "truncated part should be fetched once on rerun"
        );
        drop(guard);

        let extracted = std::fs::read_to_string(install_path.join("payload.txt")).unwrap();
        assert_eq!(extracted, "cli updater recovery");
    }

    #[compio::test]
    #[ignore = "Makes real network request"]
    async fn real_cn_endfield_patch_and_full_fallback_selection() {
        let api_client = ApiClient::new().expect("Failed to create API client");

        // Observed live behavior (2026-04-19):
        // - 1.1.9 returns patch payload for CN official.
        // - 1.2.3 does not return patch payload, so updater must use full fallback.
        let patch_case = api_client
            .get_latest_game(GameId::Endfield, ServerId::CnOfficial, Some("1.1.9"))
            .await
            .expect("get_latest_game failed for patch case");
        assert_eq!(patch_case.request_version, "1.1.9");
        assert!(
            patch_case.has_patch_package(),
            "expected patch payload for request_version=1.1.9"
        );
        assert_eq!(
            choose_update_package(&patch_case, Some("1.1.9")).expect("selection failed"),
            UpdatePackageKind::Patch
        );

        let full_case = api_client
            .get_latest_game(GameId::Endfield, ServerId::CnOfficial, Some("1.2.3"))
            .await
            .expect("get_latest_game failed for full fallback case");
        assert_eq!(full_case.request_version, "1.2.3");
        assert!(
            !full_case.has_patch_package(),
            "expected no patch payload for request_version=1.2.3"
        );
        assert_eq!(
            choose_update_package(&full_case, Some("1.2.3")).expect("selection failed"),
            UpdatePackageKind::Full
        );
    }
}
