use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::api::types::PackageInfo;
use griffr_common::config::{GameConfig, GameId, ServerId};
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

fn is_launcher_metadata_issue(path: &str) -> bool {
    matches!(
        path.replace('\\', "/").to_ascii_lowercase().as_str(),
        "game_files" | "package_files"
    )
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

fn parse_package_total_size(pkg: &PackageInfo) -> u64 {
    pkg.total_size.parse::<u64>().unwrap_or(0)
}

fn required_install_bytes(pkg: &PackageInfo) -> u64 {
    let archive_bytes: u64 = pkg.packs.iter().map(|p| p.size()).sum();
    let package_total = parse_package_total_size(pkg);
    archive_bytes.max(package_total)
}

#[cfg(windows)]
fn disk_available_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let query_path = path
        .ancestors()
        .find(|candidate| candidate.exists())
        .unwrap_or(path);
    let path_wide: Vec<u16> = query_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    let mut free_bytes_available_to_caller = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path_wide.as_ptr(),
            &mut free_bytes_available_to_caller as *mut u64,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        anyhow::bail!(
            "Failed to query free disk space for {}",
            query_path.display()
        );
    }

    Ok(free_bytes_available_to_caller)
}

#[cfg(not(windows))]
fn disk_available_bytes(_path: &Path) -> Result<u64> {
    Ok(u64::MAX)
}

fn validate_install_disk_space(pkg: &PackageInfo, install_path: &Path) -> Result<()> {
    let required_bytes = required_install_bytes(pkg);
    let available_bytes = disk_available_bytes(install_path)?;

    if available_bytes < required_bytes {
        anyhow::bail!(
            "Insufficient disk space for install at {}: required {} ({}), available {} ({}), shortfall {} ({})",
            install_path.display(),
            required_bytes,
            ui::format_bytes(required_bytes),
            available_bytes,
            ui::format_bytes(available_bytes),
            required_bytes - available_bytes,
            ui::format_bytes(required_bytes - available_bytes)
        );
    }

    Ok(())
}

pub async fn install(
    game_id: GameId,
    server_id: ServerId,
    install_path: PathBuf,
    force: bool,
    reuse_paths: Vec<PathBuf>,
    force_copy: bool,
    opts: GlobalOptions,
) -> Result<()> {
    let install_path_exists = match compio::fs::metadata(&install_path).await {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to stat install path {}", install_path.display()))
        }
    };

    if install_path_exists && !force {
        let mut entries = std::fs::read_dir(&install_path)
            .with_context(|| format!("Failed to read {}", install_path.display()))?;
        if entries.next().is_some() {
            anyhow::bail!(
                "Install path is not empty: {} (pass --force to reuse it)",
                install_path.display()
            );
        }
    }

    if opts.is_dry_run() {
        opts.dry_run(format!(
            "Would install {:?} {:?} into {}",
            game_id,
            server_id,
            install_path.display()
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

    compio::fs::create_dir_all(&install_path)
        .await
        .with_context(|| format!("Failed to create {}", install_path.display()))?;

    let api_client = ApiClient::new()?;
    let version_info = api_client
        .get_latest_game(game_id, server_id, None)
        .await
        .context("Failed to fetch version information")?;

    let pkg = version_info
        .pkg
        .as_ref()
        .context("No package information available")?;
    let total_size: u64 = pkg.packs.iter().map(|p| p.size()).sum();
    validate_install_disk_space(pkg, &install_path)?;

    ui::print_phase(format!(
        "Installing {} ({}) into {}",
        game_id,
        server_id,
        install_path.display()
    ));
    ui::print_info(format!(
        "Target version: {} | Archives: {} | Size: {}",
        version_info.version,
        pkg.packs.len(),
        ui::format_bytes(total_size)
    ));
    if !reuse_paths.is_empty() {
        ui::print_info(format!("Reuse sources: {}", reuse_paths.len()));
    }

    let mut game_config = GameConfig {
        install_path: Some(install_path.clone()),
        active_server: server_id,
        version: Some(version_info.version.clone()),
        last_update: None,
        servers: Default::default(),
    };
    let server = game_config.servers.entry(server_id).or_default();
    server.installed = true;
    server.install_path = Some(install_path.clone());
    server.version = Some(version_info.version.clone());
    let manager = GameManager::new(game_id, game_config);

    if reuse_paths.is_empty() {
        ui::print_phase("Downloading and extracting archives");
        let download_dir = install_path.join("downloads");
        compio::fs::create_dir_all(&download_dir)
            .await
            .with_context(|| format!("Failed to create {}", download_dir.display()))?;

        let mut archives: std::collections::HashMap<String, Vec<ArchivePart>> =
            std::collections::HashMap::new();
        for pack in &pkg.packs {
            let filename = pack
                .filename()
                .context("Failed to extract pack filename")?
                .split('?')
                .next()
                .unwrap_or_default()
                .to_string();
            let base = archive_base_from_url(&pack.url)
                .context("Pack URL did not end with .zip.001 or .zip")?;
            archives.entry(base).or_default().push(ArchivePart {
                url: pack.url.clone(),
                dest: download_dir.join(&filename),
                logical_path: filename,
                expected_md5: pack.md5.clone(),
                expected_size: pack.size(),
            });
        }

        let mut tasks = Vec::with_capacity(archives.len());
        for (base_name, mut parts) in archives {
            parts.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));
            tasks.push(Task::InstallArchive {
                source_dir: download_dir.clone(),
                base_name,
                dest: install_path.clone(),
                cleanup: true,
                parts,
            });
        }

        let archive_total = tasks.len();
        let archive_bar = Arc::new(StepProgress::new("install.archives", opts.verbose));
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
                "Install archive pipeline failed for {} item(s): {}",
                failures.len(),
                failures.join(", ")
            );
        }
    } else {
        ui::print_phase("Materializing files from reuse sources");
        let mut source_installs = Vec::new();
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
            let source_server_id = source.require_known_server()?;
            let source_version = source.require_config_ini_version()?.to_string();
            if source.install_path == install_path {
                continue;
            }
            source_installs.push(SourceInstallInput {
                server_id: source_server_id,
                version: source_version,
                install_path: source.install_path.clone(),
            });
        }
        let materialize_bar = Arc::new(StepProgress::new("install.materialize", opts.verbose));
        let materialize_bar_cb = materialize_bar.clone();
        let materialized = materialize_game_files_with_pool(
            &api_client,
            game_id,
            server_id,
            &version_info.version,
            &install_path,
            &pkg.file_path,
            pkg.game_files_md5.as_deref(),
            &FileReuseConfig {
                allow_copy_fallback: force_copy,
                dry_run: false,
                source_installs,
            },
            Some(|current: usize, total: usize, file: &str| {
                materialize_bar_cb.update(current, total, file);
            }),
        )
        .await
        .context("Failed to materialize files during install")?;
        materialize_bar.finish();
        ui::print_info(format!(
            "Materialized files: reused={} downloaded={}",
            materialized.reused_files, materialized.downloaded_files
        ));
        if !materialized.issues.is_empty() {
            anyhow::bail!(
                "Install materialization finished with {} issue(s)",
                materialized.issues.len()
            );
        }
    }

    manager
        .sync_launcher_metadata(&api_client)
        .await
        .context("Failed to sync launcher metadata after install staging")?;

    ui::print_phase("Verifying install integrity");
    let verify_bar = Arc::new(StepProgress::new("install.verify+repair", opts.verbose));
    let verify_bar_cb = verify_bar.clone();
    let verify_progress = move |current: usize, total: usize, file: &str| {
        verify_bar_cb.update(current, total, file);
    };
    let summary = manager
        .run_integrity_pool(&api_client, true, &[], false, Some(verify_progress))
        .await?;
    verify_bar.finish();
    if !summary.issues.is_empty() {
        let mut repairable_issues = Vec::new();
        let mut metadata_issues = Vec::new();
        for issue in summary.issues.iter().take(20) {
            ui::print_warning(format!(
                "integrity issue path={} kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
                issue.path,
                issue.kind,
                issue.expected_size,
                issue.actual_size,
                issue.expected_md5,
                issue.actual_md5
            ));
        }
        for issue in &summary.issues {
            if is_launcher_metadata_issue(&issue.path) {
                metadata_issues.push(issue.clone());
            } else {
                repairable_issues.push(issue.clone());
            }
        }

        if !metadata_issues.is_empty() {
            ui::print_info(format!(
                "Ignored metadata-only issues: {} (launcher metadata files will be normalized)",
                metadata_issues.len()
            ));
        }

        if !repairable_issues.is_empty() {
            anyhow::bail!(
                "Post-install integrity still reports {} non-metadata issue(s)",
                repairable_issues.len()
            );
        }
    }

    if !opts.skip_vfs {
        ui::print_phase("Syncing VFS resources");
        let streaming_assets = install_path
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

    ui::print_success("Install complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use griffr_common::api::types::PackFile;

    #[test]
    fn required_install_bytes_uses_larger_of_archives_and_package_total() {
        let pkg = PackageInfo {
            packs: vec![
                PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "8".to_string(),
                },
                PackFile {
                    url: "https://example.com/full.zip.002".to_string(),
                    md5: "def".to_string(),
                    package_size: "7".to_string(),
                },
            ],
            total_size: "20".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        };

        assert_eq!(required_install_bytes(&pkg), 20);
    }

    #[test]
    fn required_install_bytes_falls_back_to_archive_sum_when_total_size_invalid() {
        let pkg = PackageInfo {
            packs: vec![
                PackFile {
                    url: "https://example.com/full.zip.001".to_string(),
                    md5: "abc".to_string(),
                    package_size: "4".to_string(),
                },
                PackFile {
                    url: "https://example.com/full.zip.002".to_string(),
                    md5: "def".to_string(),
                    package_size: "6".to_string(),
                },
            ],
            total_size: "invalid".to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        };

        assert_eq!(required_install_bytes(&pkg), 10);
    }

    #[test]
    #[ignore = "Uses host filesystem to query real free disk space"]
    fn disk_available_bytes_reads_real_disk() {
        let cwd = std::env::current_dir().expect("current dir");
        let available = disk_available_bytes(&cwd).expect("query free space for cwd");
        assert!(available > 0, "expected positive available space");

        // Exercise fallback for not-yet-existing install paths.
        let deep_missing_path = cwd
            .join("griffr-test")
            .join("disk-space")
            .join("missing-target");
        let fallback_available =
            disk_available_bytes(&deep_missing_path).expect("query free space for nested path");
        assert!(
            fallback_available > 0,
            "expected positive available space for nested path"
        );
    }
}
