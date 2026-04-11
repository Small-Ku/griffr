use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::{GameConfig, GameId, ServerId};
use griffr_common::download::{DownloadOptions, Downloader};
use griffr_common::game::{
    apply_file_reuse_flow, download_vfs_resources, FileReuseConfig, GameManager, SourceInstallInput,
};

use super::local::detect_local_install;
use crate::progress::IndicatifProgress;
use crate::GlobalOptions;

fn is_launcher_metadata_issue(path: &str) -> bool {
    matches!(
        path.replace('\\', "/").to_ascii_lowercase().as_str(),
        "game_files" | "package_files"
    )
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
    if install_path.exists() && !force {
        let mut entries = tokio::fs::read_dir(&install_path)
            .await
            .with_context(|| format!("Failed to read {}", install_path.display()))?;
        if entries.next_entry().await?.is_some() {
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

    tokio::fs::create_dir_all(&install_path)
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

    println!(
        "install game={:?} server={} path={} version={} packs={} bytes={} reuse_sources={}",
        game_id,
        server_id,
        install_path.display(),
        version_info.version,
        pkg.packs.len(),
        total_size,
        reuse_paths.len()
    );

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
        let download_dir = install_path.join("downloads");
        tokio::fs::create_dir_all(&download_dir).await?;

        let downloader = Downloader::with_options(DownloadOptions {
            concurrent_connections: 4,
            retry_attempts: 3,
            resume: true,
            verify_md5: true,
        })?;
        let progress = std::sync::Arc::new(IndicatifProgress::new(total_size));
        downloader
            .download_packs(&pkg.packs, &download_dir, Some(progress))
            .await
            .context("Failed to download pack archives")?;

        let mut bases: HashSet<String> = HashSet::new();
        for pack in &pkg.packs {
            let filename = pack
                .url
                .split('/')
                .next_back()
                .and_then(|name| name.split('?').next())
                .context("Pack URL missing filename")?;
            let base = filename
                .strip_suffix(".zip.001")
                .context("Pack URL did not end with .zip.001")?;
            bases.insert(base.to_string());
        }

        let mut bases: Vec<String> = bases.into_iter().collect();
        bases.sort();

        let mut extractors = Vec::with_capacity(bases.len());
        for base in &bases {
            let extractor =
                griffr_common::download::extractor::MultiVolumeExtractor::from_directory(
                    &download_dir,
                    base,
                )?;
            extractor.extract_to(&install_path)?;
            extractors.push(extractor);
        }

        for extractor in &extractors {
            extractor.cleanup()?;
        }
    } else {
        let mut source_installs = Vec::new();
        for reuse_path in &reuse_paths {
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
            if source.install_path == install_path {
                continue;
            }
            source_installs.push(SourceInstallInput {
                server_id: source_server_id,
                version: source_version,
                install_path: source.install_path.clone(),
            });
        }
        let linked = apply_file_reuse_flow(
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
        )
        .await
        .context("Failed to apply reuse flow during install")?;
        println!("install.reused_files={}", linked);
    }

    manager
        .sync_launcher_metadata(&api_client)
        .await
        .context("Failed to sync launcher metadata after install staging")?;

    let verify_progress = |current: usize, total: usize, file: &str| {
        if opts.verbose {
            print!("\rinstall.verify {}/{} {}", current + 1, total, file);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        } else if current > 0 && current % 25 == 0 {
            print!("\rinstall.verify {}/{} {}", current, total, file);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    };
    let issues = manager
        .verify_integrity(&api_client, Some(verify_progress))
        .await?;
    println!();
    if !issues.is_empty() {
        let mut repairable_issues = Vec::new();
        let mut metadata_issues = Vec::new();
        for issue in issues.iter().take(20) {
            println!(
                "install.verify.issue path={} kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
                issue.path,
                issue.kind,
                issue.expected_size,
                issue.actual_size,
                issue.expected_md5,
                issue.actual_md5
            );
        }
        for issue in &issues {
            if is_launcher_metadata_issue(&issue.path) {
                metadata_issues.push(issue.clone());
            } else {
                repairable_issues.push(issue.clone());
            }
        }

        if !repairable_issues.is_empty() {
            println!(
                "install.verify.repairing_non_metadata_issues={}",
                repairable_issues.len()
            );
            let repair_progress = |current: usize, total: usize, file: &str| {
                println!("install.repair {}/{} {}", current + 1, total, file);
            };
            manager
                .repair_files(&api_client, &repairable_issues, Some(repair_progress))
                .await
                .context("Failed to repair post-install issues")?;
        }

        if !metadata_issues.is_empty() {
            println!(
                "install.verify.metadata_issues_ignored={} (will be normalized by launcher metadata sync)",
                metadata_issues.len()
            );
        }

        if !repairable_issues.is_empty() {
            let remaining = manager
                .verify_integrity(&api_client, None::<fn(usize, usize, &str)>)
                .await?;
            let remaining_non_metadata = remaining
                .iter()
                .filter(|i| !is_launcher_metadata_issue(&i.path))
                .count();
            if remaining_non_metadata > 0 {
                anyhow::bail!(
                    "Post-install verify still reports {} non-metadata issue(s) after repair",
                    remaining_non_metadata
                );
            }
        }
    }

    if !opts.skip_vfs {
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

    println!("install complete");
    Ok(())
}
