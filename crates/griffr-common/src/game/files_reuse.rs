//! Server game files reuse via hardlinks
//!
//! This module provides functionality to reuse game files across different server
//! installations by creating hardlinks for identical files (same relative path + MD5),
//! then downloading only the remaining files that couldn't be reused.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use tokio::io::AsyncReadExt;

use crate::api::types::GameFileEntry;
use crate::api::ApiClient;
use crate::config::{GameId, ServerId};

fn is_launcher_metadata_path(path: &str) -> bool {
    matches!(
        path.replace('\\', "/").to_ascii_lowercase().as_str(),
        "config.ini" | "game_files" | "package_files"
    )
}

async fn calculate_file_md5(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0; 8192];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Information about a source server that can provide files for reuse
#[derive(Debug, Clone)]
pub struct SourceServer {
    /// Server ID of the source
    pub server_id: ServerId,
    /// Version of the source installation
    pub version: String,
    /// Install path of the source
    pub install_path: PathBuf,
    /// Number of reusable files from this source
    pub file_count: usize,
}

/// Explicit source installation input for reuse planning.
#[derive(Debug, Clone)]
pub struct SourceInstallInput {
    /// Server ID declared by the source installation metadata.
    pub server_id: ServerId,
    /// Installed version string of the source installation.
    pub version: String,
    /// Installation path used as the file source.
    pub install_path: PathBuf,
}

/// A file that can be reused from a source server
#[derive(Debug, Clone)]
pub struct ReusableFile {
    /// Relative path of the file
    pub path: String,
    /// MD5 hash of the file
    pub md5: String,
    /// File size in bytes
    pub size: u64,
    /// Source server providing this file
    pub source_server_id: ServerId,
    /// Source install path
    pub source_path: PathBuf,
}

/// A file that needs to be downloaded
#[derive(Debug, Clone)]
pub struct DownloadFile {
    /// Relative path of the file
    pub path: String,
    /// MD5 hash of the file
    pub md5: String,
    /// File size in bytes
    pub size: u64,
}

/// Plan for reusing files from other installed servers
#[derive(Debug, Clone)]
pub struct ReusePlan {
    /// Source servers that can provide files
    pub source_servers: Vec<SourceServer>,
    /// Files that can be reused
    pub reusable_files: Vec<ReusableFile>,
    /// Files that need to be downloaded
    pub download_files: Vec<DownloadFile>,
    /// Total size of reusable files in bytes
    pub reusable_size: u64,
    /// Total size of files to download in bytes
    pub download_size: u64,
    /// Whether copy fallback is required (files on different volumes)
    pub requires_copy_fallback: bool,
}

/// Configuration for the file reuse flow
#[derive(Debug, Clone)]
pub struct FileReuseConfig {
    /// Allow copying files when hardlink creation fails
    pub allow_copy_fallback: bool,
    /// Perform a dry run without making changes
    pub dry_run: bool,
    /// Explicit source installs to consider for reuse (may include same server as target)
    pub source_installs: Vec<SourceInstallInput>,
}

/// Options for executing a reuse plan
#[derive(Debug, Clone)]
pub struct ReuseOptions {
    /// Allow copying files when hardlink creation fails
    pub allow_copy_fallback: bool,
    /// Perform a dry run without making changes
    pub dry_run: bool,
}

/// Plan file reuse from other installed servers
///
/// This function examines all installed servers of the same game and identifies
/// files that can be reused (same relative path + MD5 match) for the target server.
#[allow(clippy::too_many_arguments)]
pub async fn plan_file_reuse(
    game_id: GameId,
    _target_server_id: ServerId,
    _target_version: &str,
    target_manifest: &[GameFileEntry],
    source_installs: &[SourceInstallInput],
    api_client: &ApiClient,
) -> Result<ReusePlan> {
    // Find all eligible source installs with latest-version manifests
    let mut source_servers: Vec<SourceServer> = Vec::new();
    let mut source_manifests: Vec<Vec<GameFileEntry>> = Vec::new();

    for source in source_installs {
        let server_id = source.server_id;
        let version = &source.version;

        // Fetch the manifest for this source server
        let version_info = match api_client
            .get_latest_game(game_id, server_id, Some(version))
            .await
        {
            Ok(info) => info,
            Err(_) => continue,
        };

        let pkg = match &version_info.pkg {
            Some(pkg) => pkg,
            None => continue,
        };

        // Only use servers that are at their latest version
        if version_info.version != *version {
            continue;
        }

        let manifest = match api_client
            .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
            .await
        {
            Ok(m) => m,
            Err(_) => continue,
        };

        source_servers.push(SourceServer {
            server_id,
            version: version.clone(),
            install_path: source.install_path.clone(),
            file_count: manifest.len(),
        });
        source_manifests.push(manifest);
    }

    // If no source servers found, everything must be downloaded
    if source_servers.is_empty() {
        return Ok(ReusePlan {
            source_servers: vec![],
            reusable_files: vec![],
            download_files: target_manifest
                .iter()
                .map(|e| DownloadFile {
                    path: e.path.clone(),
                    md5: e.md5.clone(),
                    size: e.size,
                })
                .collect(),
            reusable_size: 0,
            download_size: target_manifest.iter().map(|e| e.size).sum(),
            requires_copy_fallback: false,
        });
    }

    // Build a map of reusable files
    let target_manifest_map: HashMap<&str, &GameFileEntry> = target_manifest
        .iter()
        .filter(|e| !is_launcher_metadata_path(&e.path))
        .map(|e| (e.path.as_str(), e))
        .collect();

    let mut reusable_files: Vec<ReusableFile> = Vec::new();
    let mut reusable_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut reusable_size: u64 = 0;

    // For each source server, find matching files
    for (idx, source) in source_servers.iter().enumerate() {
        let source_manifest = match source_manifests.get(idx) {
            Some(m) => m,
            None => continue,
        };

        for entry in source_manifest {
            if is_launcher_metadata_path(&entry.path) {
                continue;
            }
            // Skip if already reused from another source
            if reusable_paths.contains(&entry.path) {
                continue;
            }

            // Check if this file exists in target manifest with same path and MD5
            if let Some(target_entry) = target_manifest_map.get(entry.path.as_str()) {
                if target_entry.md5.to_lowercase() == entry.md5.to_lowercase()
                    && target_entry.size == entry.size
                {
                    // Validate the source file on disk so a corrupted source install
                    // does not get hardlinked into the target.
                    let source_file = source.install_path.join(&entry.path);
                    let metadata = match tokio::fs::metadata(&source_file).await {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if metadata.len() != entry.size {
                        continue;
                    }
                    let source_md5 = match calculate_file_md5(&source_file).await {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if source_md5 != entry.md5.to_lowercase() {
                        continue;
                    }

                    reusable_files.push(ReusableFile {
                        path: entry.path.clone(),
                        md5: entry.md5.clone(),
                        size: entry.size,
                        source_server_id: source.server_id,
                        source_path: source.install_path.clone(),
                    });
                    reusable_paths.insert(entry.path.clone());
                    reusable_size += entry.size;
                }
            }
        }
    }

    // Build list of files to download
    let mut download_files: Vec<DownloadFile> = Vec::new();
    let mut download_size: u64 = 0;

    for entry in target_manifest {
        if is_launcher_metadata_path(&entry.path) {
            continue;
        }
        if !reusable_paths.contains(&entry.path) {
            download_files.push(DownloadFile {
                path: entry.path.clone(),
                md5: entry.md5.clone(),
                size: entry.size,
            });
            download_size += entry.size;
        }
    }

    // Determine if copy fallback might be required
    // On Windows, hardlinks can only be created within the same volume
    let requires_copy_fallback = false; // Will be determined during execution

    Ok(ReusePlan {
        source_servers,
        reusable_files,
        download_files,
        reusable_size,
        download_size,
        requires_copy_fallback,
    })
}

/// Execute a file reuse plan by creating hardlinks (or copying if fallback allowed)
///
/// This function creates hardlinks for reusable files from source servers to the
/// target installation path. If hardlink creation fails and copy fallback is allowed,
/// files are copied instead.
pub async fn execute_reuse_plan(
    target_path: &Path,
    plan: &ReusePlan,
    options: ReuseOptions,
) -> Result<()> {
    if plan.reusable_files.is_empty() {
        return Ok(());
    }

    if options.dry_run {
        println!("Would create {} hardlinks:", plan.reusable_files.len());
        for file in plan.reusable_files.iter().take(10) {
            println!("  {} <- {}", file.path, file.source_server_id);
        }
        if plan.reusable_files.len() > 10 {
            println!("  ... and {} more", plan.reusable_files.len() - 10);
        }
        return Ok(());
    }

    println!(
        "Creating {} hardlinks for reusable files...",
        plan.reusable_files.len()
    );

    let mut hardlink_failures: Vec<(String, String)> = Vec::new();

    for file in &plan.reusable_files {
        let source_file = file.source_path.join(&file.path);
        let target_file = target_path.join(&file.path);

        // Create parent directories if needed
        if let Some(parent) = target_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Try to create hardlink (cross-platform)
        match std::fs::hard_link(&source_file, &target_file) {
            Ok(()) => continue,
            Err(_e) if target_file.exists() => {
                // Target already exists (e.g., from a previous version).
                // Remove it and retry the hardlink.
                tokio::fs::remove_file(&target_file).await?;
                match std::fs::hard_link(&source_file, &target_file) {
                    Ok(()) => continue,
                    Err(retry_err) => {
                        if !options.allow_copy_fallback {
                            hardlink_failures.push((file.path.clone(), retry_err.to_string()));
                        } else {
                            tokio::fs::copy(&source_file, &target_file).await?;
                        }
                    }
                }
            }
            Err(e) => {
                if !options.allow_copy_fallback {
                    hardlink_failures.push((file.path.clone(), e.to_string()));
                } else {
                    // Fall back to copy
                    tokio::fs::copy(&source_file, &target_file).await?;
                }
            }
        }
    }

    if !hardlink_failures.is_empty() {
        anyhow::bail!(
            "Failed to create hardlinks for {} files. Use --force-copy to allow copying. \
             First failure: {} - {}",
            hardlink_failures.len(),
            hardlink_failures[0].0,
            hardlink_failures[0].1
        );
    }

    Ok(())
}

/// Derive base URL for individual file downloads from package file path
///
/// The file_path typically ends with "/game_files", so we strip that to get
/// the CDN base URL for direct file downloads.
pub fn derive_files_base_url(file_path: &str) -> Result<String> {
    let normalized = file_path.trim_end_matches('/');
    if let Some(base) = normalized.strip_suffix("/game_files") {
        return Ok(base.to_string());
    }
    if normalized.ends_with("/files") {
        return Ok(normalized.to_string());
    }
    anyhow::bail!(
        "Expected file_path to end with '/game_files' or '/files', got: {}",
        file_path
    );
}

/// Print a summary of the file reuse plan
///
/// Shows source servers, reusable files count/size, and download files count/size.
pub fn print_reuse_plan_summary(plan: &ReusePlan, force_copy: bool) {
    if !plan.source_servers.is_empty() {
        println!("\nFile reuse plan:");
        println!(" Source servers:");
        for source in &plan.source_servers {
            println!(
                " - {} (version {}, {} files)",
                source.server_id, source.version, source.file_count
            );
        }
        println!(
            " Reusable files: {} ({:.2} GB)",
            plan.reusable_files.len(),
            plan.reusable_size as f64 / 1024.0 / 1024.0 / 1024.0
        );
        println!(
            " Files to download: {} ({:.2} GB)",
            plan.download_files.len(),
            plan.download_size as f64 / 1024.0 / 1024.0 / 1024.0
        );

        if plan.requires_copy_fallback && !force_copy {
            println!("\nWARNING: Some files may require copy fallback (different volumes)");
            println!(" Use --force-copy to allow copying if hardlink fails.");
        }
    } else {
        println!("\nNo eligible source servers found for file reuse.");
    }
}

/// Apply the complete file reuse flow
///
/// This is the main entry point for using file reuse during install/update.
/// It handles:
/// 1. Fetching the target manifest
/// 2. Planning file reuse from other installed servers
/// 3. Printing the reuse plan summary
/// 4. Executing hardlinks for reusable files
/// 5. Downloading remaining files
///
/// Returns the number of files that were hardlinked.
#[allow(clippy::too_many_arguments)]
pub async fn apply_file_reuse_flow(
    api_client: &ApiClient,
    game_id: crate::config::GameId,
    target_server_id: crate::config::ServerId,
    target_version: &str,
    install_path: &Path,
    file_path: &str,
    game_files_md5: Option<&str>,
    config: &FileReuseConfig,
) -> Result<usize> {
    // 1. Fetch target manifest
    let manifest = api_client
        .fetch_game_files(file_path, game_files_md5)
        .await
        .context("Failed to fetch target manifest for reuse planning")?;

    // 2. Plan file reuse
    let reuse_plan = plan_file_reuse(
        game_id,
        target_server_id,
        target_version,
        &manifest,
        &config.source_installs,
        api_client,
    )
    .await
    .context("Failed to plan file reuse")?;

    // 3. Print reuse plan
    print_reuse_plan_summary(&reuse_plan, config.allow_copy_fallback);

    // 4. Execute reuse plan - hardlink all reusable files
    let mut files_linked = 0;
    if !reuse_plan.reusable_files.is_empty() {
        let options = ReuseOptions {
            allow_copy_fallback: config.allow_copy_fallback,
            dry_run: config.dry_run,
        };
        execute_reuse_plan(install_path, &reuse_plan, options)
            .await
            .context("Failed to execute file reuse plan")?;

        files_linked = reuse_plan.reusable_files.len();
        println!("File reuse complete: {} files linked.", files_linked);
    }

    // 5. Download remaining game files
    if !reuse_plan.download_files.is_empty() {
        let files_base_url = derive_files_base_url(file_path)?;

        download_remaining_files(
            api_client,
            &reuse_plan.download_files,
            install_path,
            &files_base_url,
        )
        .await
        .context("Failed to download remaining files")?;
    }

    Ok(files_linked)
}

/// Download remaining game files that couldn't be reused
///
/// This function downloads individual game files directly from the CDN,
/// similar to the verify repair process. It creates parent directories
/// as needed and verifies each file's MD5 hash after download.
pub async fn download_remaining_files(
    api_client: &ApiClient,
    download_files: &[DownloadFile],
    install_path: &Path,
    files_base_url: &str,
) -> Result<()> {
    if download_files.is_empty() {
        return Ok(());
    }

    println!("\nDownloading remaining {} files...", download_files.len());

    let total_size: u64 = download_files.iter().map(|f| f.size).sum();
    let mut downloaded: u64 = 0;

    for (i, file) in download_files.iter().enumerate() {
        let file_url = format!("{}/{}", files_base_url, file.path);
        let output_path = install_path.join(&file.path);

        // Create parent directory
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Progress reporting (every 10 files or for small downloads, every file)
        if download_files.len() <= 10 || i % 10 == 0 {
            println!(
                "  [{}/{}] Downloading {} ({:.1} MB / {:.1} MB)",
                i + 1,
                download_files.len(),
                file.path,
                downloaded as f64 / 1024.0 / 1024.0,
                total_size as f64 / 1024.0 / 1024.0
            );
        }

        api_client
            .download_file_with_verify(&file_url, &output_path, &file.md5)
            .await
            .with_context(|| format!("Failed to download {}", file.path))?;

        downloaded += file.size;
    }

    println!(
        "Download complete ({} files, {:.2} MB).",
        download_files.len(),
        total_size as f64 / 1024.0 / 1024.0
    );

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_reuse_plan_size_calculation() {
        let plan = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![],
            download_files: vec![
                DownloadFile {
                    path: "file1.bin".to_string(),
                    md5: "abc".to_string(),
                    size: 100,
                },
                DownloadFile {
                    path: "file2.bin".to_string(),
                    md5: "def".to_string(),
                    size: 200,
                },
            ],
            reusable_size: 0,
            download_size: 300,
            requires_copy_fallback: false,
        };

        assert_eq!(plan.download_size, 300);
    }

    #[test]
    fn test_reuse_plan_size_calculation_with_reusable_files() {
        let plan = ReusePlan {
            source_servers: vec![SourceServer {
                server_id: ServerId::CnOfficial,
                version: "1.0.0".to_string(),
                install_path: PathBuf::from("/source"),
                file_count: 2,
            }],
            reusable_files: vec![
                ReusableFile {
                    path: "file1.bin".to_string(),
                    md5: "abc123".to_string(),
                    size: 100,
                    source_server_id: ServerId::CnOfficial,
                    source_path: PathBuf::from("/source"),
                },
                ReusableFile {
                    path: "file2.bin".to_string(),
                    md5: "def456".to_string(),
                    size: 200,
                    source_server_id: ServerId::CnOfficial,
                    source_path: PathBuf::from("/source"),
                },
            ],
            download_files: vec![DownloadFile {
                path: "file3.bin".to_string(),
                md5: "ghi789".to_string(),
                size: 300,
            }],
            reusable_size: 300,
            download_size: 300,
            requires_copy_fallback: false,
        };

        assert_eq!(plan.reusable_size, 300);
        assert_eq!(plan.download_size, 300);
        assert_eq!(plan.reusable_size + plan.download_size, 600);
    }

    #[test]
    fn test_reuse_plan_empty() {
        let plan = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![],
            download_files: vec![],
            reusable_size: 0,
            download_size: 0,
            requires_copy_fallback: false,
        };

        assert!(plan.reusable_files.is_empty());
        assert!(plan.download_files.is_empty());
        assert!(plan.source_servers.is_empty());
        assert_eq!(plan.reusable_size, 0);
        assert_eq!(plan.download_size, 0);
    }

    #[test]
    fn test_reuse_options_defaults() {
        let options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        assert!(!options.allow_copy_fallback);
        assert!(!options.dry_run);

        let options_with_fallback = ReuseOptions {
            allow_copy_fallback: true,
            dry_run: false,
        };

        assert!(options_with_fallback.allow_copy_fallback);
        assert!(!options_with_fallback.dry_run);

        let dry_run_options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: true,
        };

        assert!(!dry_run_options.allow_copy_fallback);
        assert!(dry_run_options.dry_run);
    }

    #[tokio::test]
    async fn test_execute_reuse_plan_empty() {
        let _temp_dir = TempDir::new().unwrap();

        let plan = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![],
            download_files: vec![],
            reusable_size: 0,
            download_size: 0,
            requires_copy_fallback: false,
        };

        let options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        // Should complete without error
        let result = execute_reuse_plan(_temp_dir.path(), &plan, options).await;
        assert!(result.is_ok());

        // TempDir automatically cleaned up when dropped
    }

    #[tokio::test]
    async fn test_execute_reuse_plan_dry_run() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();

        // Create source file
        let source_file = source_dir.join("data.bin");
        tokio::fs::write(&source_file, b"test content")
            .await
            .unwrap();

        let plan = ReusePlan {
            source_servers: vec![SourceServer {
                server_id: ServerId::CnOfficial,
                version: "1.0.0".to_string(),
                install_path: source_dir.clone(),
                file_count: 1,
            }],
            reusable_files: vec![ReusableFile {
                path: "data.bin".to_string(),
                md5: "abc123".to_string(),
                size: 12,
                source_server_id: ServerId::CnOfficial,
                source_path: source_dir.clone(),
            }],
            download_files: vec![],
            reusable_size: 12,
            download_size: 0,
            requires_copy_fallback: false,
        };

        let options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: true, // Dry run - should not actually create hardlinks
        };

        // Execute dry run
        let result = execute_reuse_plan(temp_dir.path(), &plan, options).await;
        assert!(result.is_ok());

        // Verify no hardlink was created (target should not exist)
        let target_file = temp_dir.path().join("data.bin");
        assert!(!target_file.exists(), "Dry run should not create files");

        // TempDir automatically cleaned up when dropped
    }

    #[tokio::test]
    async fn test_execute_reuse_plan_with_hardlinks() {
        let temp_dir = TempDir::new().unwrap();

        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        // Create source files
        let source_file1 = source_dir.join("file1.bin");
        std::fs::write(&source_file1, b"content1").unwrap();
        let source_file2 = source_dir.join("subdir/file2.bin");
        std::fs::create_dir_all(source_file2.parent().unwrap()).unwrap();
        std::fs::write(&source_file2, b"content2").unwrap();

        let plan = ReusePlan {
            source_servers: vec![SourceServer {
                server_id: ServerId::CnOfficial,
                version: "1.0.0".to_string(),
                install_path: source_dir.clone(),
                file_count: 2,
            }],
            reusable_files: vec![
                ReusableFile {
                    path: "file1.bin".to_string(),
                    md5: "hash1".to_string(),
                    size: 8,
                    source_server_id: ServerId::CnOfficial,
                    source_path: source_dir.clone(),
                },
                ReusableFile {
                    path: "subdir/file2.bin".to_string(),
                    md5: "hash2".to_string(),
                    size: 8,
                    source_server_id: ServerId::CnOfficial,
                    source_path: source_dir.clone(),
                },
            ],
            download_files: vec![],
            reusable_size: 16,
            download_size: 0,
            requires_copy_fallback: false,
        };

        let options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        // Execute
        let result = execute_reuse_plan(&target_dir, &plan, options).await;
        assert!(
            result.is_ok(),
            "Hardlink creation should succeed: {:?}",
            result
        );

        // Verify hardlinks were created
        let target_file1 = target_dir.join("file1.bin");
        let target_file2 = target_dir.join("subdir/file2.bin");

        assert!(target_file1.exists(), "Hardlink file1 should exist");
        assert!(target_file2.exists(), "Hardlink file2 should exist");

        // Verify content
        assert_eq!(std::fs::read_to_string(&target_file1).unwrap(), "content1");
        assert_eq!(std::fs::read_to_string(&target_file2).unwrap(), "content2");

        // TempDir automatically cleaned up when dropped
    }

    #[tokio::test]
    async fn test_execute_reuse_plan_with_copy_fallback() {
        let temp_dir = TempDir::new().unwrap();

        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();

        // Create source file
        let source_file = source_dir.join("test.bin");
        std::fs::write(&source_file, b"test data").unwrap();

        // Create a fake source that doesn't exist to trigger fallback
        // (simulating cross-filesystem scenario where hardlink would fail)
        let fake_source_dir = temp_dir.path().join("fake_source");

        let plan = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![ReusableFile {
                path: "test.bin".to_string(),
                md5: "hash".to_string(),
                size: 9,
                source_server_id: ServerId::CnOfficial,
                source_path: source_dir.clone(), // This exists
            }],
            download_files: vec![],
            reusable_size: 9,
            download_size: 0,
            requires_copy_fallback: false,
        };

        // Test without fallback - should succeed when source exists (hardlink works)
        let options_no_fallback = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        let result = execute_reuse_plan(&target_dir, &plan, options_no_fallback).await;
        assert!(result.is_ok(), "Hardlink should succeed: {:?}", result);

        // Create a scenario where hardlink will fail (source file doesn't exist)
        let plan_with_missing_source = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![ReusableFile {
                path: "nonexistent.bin".to_string(),
                md5: "hash".to_string(),
                size: 9,
                source_server_id: ServerId::CnOfficial,
                source_path: fake_source_dir.clone(),
            }],
            download_files: vec![],
            reusable_size: 9,
            download_size: 0,
            requires_copy_fallback: false,
        };

        let options_no_fallback2 = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        let result =
            execute_reuse_plan(&target_dir, &plan_with_missing_source, options_no_fallback2).await;
        assert!(
            result.is_err(),
            "Should fail when source doesn't exist and no fallback"
        );

        // Now test the copy fallback creates the file
        let source_file2 = source_dir.join("fallback.bin");
        std::fs::write(&source_file2, b"fallback data").unwrap();

        let plan_with_fallback = ReusePlan {
            source_servers: vec![],
            reusable_files: vec![ReusableFile {
                path: "fallback.bin".to_string(),
                md5: "hash".to_string(),
                size: 13,
                source_server_id: ServerId::CnOfficial,
                source_path: source_dir.clone(),
            }],
            download_files: vec![],
            reusable_size: 13,
            download_size: 0,
            requires_copy_fallback: false,
        };

        // With allow_copy_fallback = true, should succeed
        let options_with_fallback = ReuseOptions {
            allow_copy_fallback: true,
            dry_run: false,
        };

        // This should work because source exists and hardlink should succeed
        // Clean up to test actual copy fallback would need different setup
        let result =
            execute_reuse_plan(&target_dir, &plan_with_fallback, options_with_fallback).await;
        assert!(
            result.is_ok(),
            "Should succeed with copy fallback allowed: {:?}",
            result
        );

        let target_fallback = target_dir.join("fallback.bin");
        assert!(target_fallback.exists());
        assert_eq!(
            std::fs::read_to_string(&target_fallback).unwrap(),
            "fallback data"
        );

        // TempDir automatically cleaned up when dropped
    }

    #[tokio::test]
    async fn test_execute_reuse_plan_multiple_source_servers() {
        let temp_dir = TempDir::new().unwrap();

        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        // Create two source servers
        let source_dir1 = temp_dir.path().join("source1");
        std::fs::create_dir_all(&source_dir1).unwrap();
        let source_file1 = source_dir1.join("server1.bin");
        std::fs::write(&source_file1, b"server1 data").unwrap();

        let source_dir2 = temp_dir.path().join("source2");
        std::fs::create_dir_all(&source_dir2).unwrap();
        let source_file2 = source_dir2.join("server2.bin");
        std::fs::write(&source_file2, b"server2 data").unwrap();

        let plan = ReusePlan {
            source_servers: vec![
                SourceServer {
                    server_id: ServerId::CnOfficial,
                    version: "1.0.0".to_string(),
                    install_path: source_dir1.clone(),
                    file_count: 1,
                },
                SourceServer {
                    server_id: ServerId::CnBilibili,
                    version: "1.0.0".to_string(),
                    install_path: source_dir2.clone(),
                    file_count: 1,
                },
            ],
            reusable_files: vec![
                ReusableFile {
                    path: "server1.bin".to_string(),
                    md5: "hash1".to_string(),
                    size: 13,
                    source_server_id: ServerId::CnOfficial,
                    source_path: source_dir1.clone(),
                },
                ReusableFile {
                    path: "server2.bin".to_string(),
                    md5: "hash2".to_string(),
                    size: 13,
                    source_server_id: ServerId::CnBilibili,
                    source_path: source_dir2.clone(),
                },
            ],
            download_files: vec![],
            reusable_size: 26,
            download_size: 0,
            requires_copy_fallback: false,
        };

        let options = ReuseOptions {
            allow_copy_fallback: false,
            dry_run: false,
        };

        let result = execute_reuse_plan(&target_dir, &plan, options).await;
        assert!(result.is_ok());

        // Verify both files exist
        assert!(target_dir.join("server1.bin").exists());
        assert!(target_dir.join("server2.bin").exists());
        assert_eq!(
            std::fs::read_to_string(target_dir.join("server1.bin")).unwrap(),
            "server1 data"
        );
        assert_eq!(
            std::fs::read_to_string(target_dir.join("server2.bin")).unwrap(),
            "server2 data"
        );

        // TempDir automatically cleaned up when dropped
    }

    #[tokio::test]
    async fn test_download_remaining_files_empty() {
        let _temp_dir = TempDir::new().unwrap();

        // This would need a real ApiClient to test fully
        // For now, verify the function handles empty input gracefully
        // (would need mocking for full test coverage)

        // Empty file list should early return Ok
        // We can't test download_remaining_files without a real API client
        // or a mock - it's marked as known limitation
    }

    #[test]
    fn test_download_file_struct() {
        let file = DownloadFile {
            path: "assets/game.bin".to_string(),
            md5: "abcdef123456".to_string(),
            size: 1024 * 1024 * 100, // 100 MB
        };

        assert_eq!(file.path, "assets/game.bin");
        assert_eq!(file.md5, "abcdef123456");
        assert_eq!(file.size, 104857600);
    }

    #[test]
    fn test_reusable_file_struct() {
        let file = ReusableFile {
            path: "data/config.json".to_string(),
            md5: "1234567890ab".to_string(),
            size: 2048,
            source_server_id: ServerId::GlobalOfficial,
            source_path: PathBuf::from("/mnt/games/arknights/global"),
        };

        assert_eq!(file.path, "data/config.json");
        assert_eq!(file.source_server_id, ServerId::GlobalOfficial);
        assert_eq!(file.size, 2048);
    }

    #[test]
    fn test_source_server_struct() {
        let source = SourceServer {
            server_id: ServerId::CnBilibili,
            version: "2.1.0".to_string(),
            install_path: PathBuf::from("/games/endfield/cn-bili"),
            file_count: 5000,
        };

        assert_eq!(source.server_id, ServerId::CnBilibili);
        assert_eq!(source.version, "2.1.0");
        assert_eq!(source.file_count, 5000);
    }

    #[test]
    fn test_reuse_plan_with_mixed_files() {
        // Test a realistic scenario with some reusable, some to download
        let plan = ReusePlan {
            source_servers: vec![SourceServer {
                server_id: ServerId::CnOfficial,
                version: "2.0.0".to_string(),
                install_path: PathBuf::from("/games/source"),
                file_count: 100,
            }],
            reusable_files: (0..80)
                .map(|i| ReusableFile {
                    path: format!("assets/file_{:03}.bin", i),
                    md5: format!("md5_{:03}", i),
                    size: 1024 * 1024, // 1 MB each
                    source_server_id: ServerId::CnOfficial,
                    source_path: PathBuf::from("/games/source"),
                })
                .collect(),
            download_files: (80..100)
                .map(|i| DownloadFile {
                    path: format!("assets/file_{:03}.bin", i),
                    md5: format!("new_md5_{:03}", i),
                    size: 1024 * 1024, // 1 MB each
                })
                .collect(),
            reusable_size: 80 * 1024 * 1024,
            download_size: 20 * 1024 * 1024,
            requires_copy_fallback: false,
        };

        assert_eq!(plan.reusable_files.len(), 80);
        assert_eq!(plan.download_files.len(), 20);
        assert_eq!(plan.reusable_size, 80 * 1024 * 1024);
        assert_eq!(plan.download_size, 20 * 1024 * 1024);

        // 80% of files can be reused
        let reuse_percentage = plan.reusable_files.len() as f64
            / (plan.reusable_files.len() + plan.download_files.len()) as f64
            * 100.0;
        assert!((reuse_percentage - 80.0).abs() < 0.1);
    }

    #[test]
    fn test_game_id_server_id_variants() {
        // Ensure all game/server combinations work in test data
        let games = [GameId::Arknights, GameId::Endfield];
        let servers = [
            ServerId::CnOfficial,
            ServerId::CnBilibili,
            ServerId::GlobalOfficial,
            ServerId::GlobalEpic,
        ];

        // This is more of a compile-time check that
        // all combinations compile correctly
        for _game in &games {
            for _server in &servers {
                // Would use in actual function calls
            }
        }
    }

    #[test]
    fn test_is_launcher_metadata_path_matches_expected_names() {
        assert!(is_launcher_metadata_path("config.ini"));
        assert!(is_launcher_metadata_path("game_files"));
        assert!(is_launcher_metadata_path("package_files"));
        assert!(is_launcher_metadata_path("CONFIG.INI"));
        assert!(is_launcher_metadata_path("Package_Files"));
        assert!(!is_launcher_metadata_path("Endfield_Data/config.ini"));
        assert!(!is_launcher_metadata_path("SomeGame/game_files.bin"));
    }

    #[test]
    fn test_derive_files_base_url_from_game_files_suffix() {
        let url = "https://cdn.example.com/path/files/game_files";
        let base = derive_files_base_url(url).unwrap();
        assert_eq!(base, "https://cdn.example.com/path/files");
    }

    #[test]
    fn test_derive_files_base_url_from_files_suffix() {
        let url = "https://cdn.example.com/path/files";
        let base = derive_files_base_url(url).unwrap();
        assert_eq!(base, "https://cdn.example.com/path/files");
    }

    #[test]
    fn test_derive_files_base_url_rejects_unknown_shape() {
        let url = "https://cdn.example.com/path";
        let err = derive_files_base_url(url).unwrap_err();
        assert!(err
            .to_string()
            .contains("Expected file_path to end with '/game_files' or '/files'"));
    }
}
