use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::config::GameId;
use griffr_common::download::{DownloadOptions, Downloader};
use griffr_common::game::{
    apply_file_reuse_flow, download_vfs_resources, FileReuseConfig, GameManager, SourceInstallInput,
};

use super::local::detect_local_install;
use crate::progress::IndicatifProgress;
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

fn archive_bases_from_urls<'a>(urls: impl IntoIterator<Item = &'a str>) -> Result<Vec<String>> {
    let mut bases: HashSet<String> = HashSet::new();
    for url in urls {
        if let Some(base) = archive_base_from_url(url) {
            bases.insert(base);
        }
    }
    if bases.is_empty() {
        anyhow::bail!("Could not determine archive base name from pack URLs");
    }
    let mut bases: Vec<String> = bases.into_iter().collect();
    bases.sort();
    Ok(bases)
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

    let issues = manager
        .verify_integrity(api_client, None::<fn(usize, usize, &str)>)
        .await?;
    println!("update.verify.issues={}", issues.len());
    for issue in issues.iter().take(20) {
        println!("{} {:?}", issue.path, issue.kind);
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

    let linked = apply_file_reuse_flow(
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
    )
    .await?;

    println!("update.reused_files={}", linked);
    verify_updated_install(api_client, manager, &version_info.version, opts.skip_verify).await?;
    Ok(())
}

async fn download_and_extract_archives(
    archives: &[griffr_common::api::types::PackFile],
    install_path: &Path,
    label: &str,
    opts: &GlobalOptions,
) -> Result<Vec<griffr_common::download::extractor::MultiVolumeExtractor>> {
    let total_size: u64 = archives.iter().map(|p| p.size()).sum();
    println!("update.label={} bytes={}", label, total_size);

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
        .download_packs(archives, &download_dir, Some(progress))
        .await?;

    let bases = archive_bases_from_urls(archives.iter().map(|p| p.url.as_str()))?;
    let mut extractors = Vec::with_capacity(bases.len());
    for base in &bases {
        opts.verbose(format!("extracting archive base {}", base));
        let extractor = griffr_common::download::extractor::MultiVolumeExtractor::from_directory(
            &download_dir,
            base,
        )?;
        extractor.extract_to(install_path)?;
        extractors.push(extractor);
    }
    Ok(extractors)
}

fn validate_patch_target(game_id: GameId, install_path: &Path) -> Result<()> {
    let expected_exe = install_path.join(match game_id {
        GameId::Arknights => "Arknights.exe",
        GameId::Endfield => "Endfield.exe",
    });
    if !expected_exe.exists() {
        anyhow::bail!("Patch target missing {}", expected_exe.display());
    }
    Ok(())
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

    println!(
        "update path={} game={:?} server={} current={} latest={}",
        local.install_path.display(),
        game_id,
        server_id,
        current_version,
        version_info.version
    );

    if current_version == version_info.version || !version_info.has_update() {
        println!("update noop");
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

        println!("update complete");
        return Ok(());
    }

    match package_kind {
        UpdatePackageKind::Patch => {
            validate_patch_target(game_id, &local.install_path)?;
            let patch = version_info
                .patch
                .as_ref()
                .context("No patch package information available")?;
            let extractors =
                download_and_extract_archives(&patch.patches, &local.install_path, "patch", &opts)
                    .await?;
            verify_updated_install(
                &api_client,
                &mut manager,
                &version_info.version,
                opts.skip_verify,
            )
            .await?;
            for extractor in &extractors {
                extractor.cleanup()?;
            }
        }
        UpdatePackageKind::Full => {
            let pkg = version_info
                .pkg
                .as_ref()
                .context("No full package information available")?;
            let extractors =
                download_and_extract_archives(&pkg.packs, &local.install_path, "full", &opts)
                    .await?;
            verify_updated_install(
                &api_client,
                &mut manager,
                &version_info.version,
                opts.skip_verify,
            )
            .await?;
            for extractor in &extractors {
                extractor.cleanup()?;
            }
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

    println!("update complete");
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
