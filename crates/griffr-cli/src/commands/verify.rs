use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::client::ApiClient;
use griffr_common::game::{execute_reuse_plan, plan_file_reuse, ReuseOptions, SourceInstallInput};

use super::local::detect_local_install;
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

    println!(
        "verify path={} game={:?} server={} version={}",
        local.install_path.display(),
        game_id,
        server_id,
        installed_version
    );

    let progress_cb = |current: usize, total: usize, file: &str| {
        if opts.verbose {
            print!("\rverify {}/{} {}", current + 1, total, file);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        } else if current > 0 && current % 25 == 0 {
            print!("\rverify {}/{} {}", current, total, file);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    };

    let issues = manager
        .verify_integrity(&api_client, Some(progress_cb))
        .await
        .context("verify_integrity failed")?;
    println!("\nverify issues={}", issues.len());

    if issues.is_empty() {
        if repair {
            manager
                .sync_launcher_metadata(&api_client)
                .await
                .context("Failed to sync launcher metadata")?;
        }
        return Ok(());
    }

    for issue in &issues {
        println!(
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
        let downloaded;
        if !reuse_paths.is_empty() {
            let version_info = api_client
                .get_latest_game(game_id, server_id, Some(&installed_version))
                .await?;
            let pkg = version_info
                .pkg
                .as_ref()
                .context("No package information available")?;
            let target_manifest = api_client
                .fetch_game_files(&pkg.file_path, pkg.game_files_md5.as_deref())
                .await?;

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
            let reuse_plan = plan_file_reuse(
                game_id,
                server_id,
                &installed_version,
                &target_manifest,
                &source_installs,
                &api_client,
            )
            .await
            .context("Failed to plan file reuse for repair")?;

            let issue_paths: HashSet<&str> = issues.iter().map(|i| i.path.as_str()).collect();
            let reusable_files: Vec<_> = reuse_plan
                .reusable_files
                .into_iter()
                .filter(|f| issue_paths.contains(f.path.as_str()))
                .collect();

            let reused_paths: HashSet<String> =
                reusable_files.iter().map(|f| f.path.clone()).collect();
            if !reusable_files.is_empty() {
                let scoped_plan = griffr_common::game::ReusePlan {
                    source_servers: reuse_plan.source_servers,
                    reusable_files,
                    download_files: Vec::new(),
                    reusable_size: 0,
                    download_size: 0,
                    requires_copy_fallback: false,
                };
                execute_reuse_plan(
                    &local.install_path,
                    &scoped_plan,
                    ReuseOptions {
                        allow_copy_fallback: force_copy,
                        dry_run: false,
                    },
                )
                .await
                .context("Failed to reuse local files during repair")?;
                println!("repair.reused_files={}", reused_paths.len());
            }

            let remaining_issues: Vec<_> = issues
                .iter()
                .filter(|issue| !reused_paths.contains(&issue.path))
                .cloned()
                .collect();
            let metadata_issues: Vec<_> = remaining_issues
                .iter()
                .filter(|issue| is_launcher_metadata_issue(&issue.path))
                .cloned()
                .collect();
            let downloadable_issues: Vec<_> = remaining_issues
                .iter()
                .filter(|issue| !is_launcher_metadata_issue(&issue.path))
                .cloned()
                .collect();
            if !metadata_issues.is_empty() {
                println!(
                    "repair.metadata_issues_ignored={} (metadata will be normalized by launcher sync)",
                    metadata_issues.len()
                );
            }
            downloaded = downloadable_issues.len();
            if !downloadable_issues.is_empty() {
                let repair_progress = |current: usize, total: usize, file: &str| {
                    println!("repair {}/{} {}", current + 1, total, file);
                };
                manager
                    .repair_files(&api_client, &downloadable_issues, Some(repair_progress))
                    .await?;
            }
        } else {
            let metadata_issues: Vec<_> = issues
                .iter()
                .filter(|issue| is_launcher_metadata_issue(&issue.path))
                .cloned()
                .collect();
            let downloadable_issues: Vec<_> = issues
                .iter()
                .filter(|issue| !is_launcher_metadata_issue(&issue.path))
                .cloned()
                .collect();
            if !metadata_issues.is_empty() {
                println!(
                    "repair.metadata_issues_ignored={} (metadata will be normalized by launcher sync)",
                    metadata_issues.len()
                );
            }
            downloaded = downloadable_issues.len();
            if !downloadable_issues.is_empty() {
                let repair_progress = |current: usize, total: usize, file: &str| {
                    println!("repair {}/{} {}", current + 1, total, file);
                };
                manager
                    .repair_files(&api_client, &downloadable_issues, Some(repair_progress))
                    .await?;
            }
        }

        println!("repair.downloaded_files={}", downloaded);
        println!("repair.syncing_metadata=started");
        manager
            .sync_launcher_metadata(&api_client)
            .await
            .context("Failed to sync launcher metadata after repair")?;
        println!("repair.syncing_metadata=done");

        let post_progress = |current: usize, total: usize, file: &str| {
            if opts.verbose {
                print!("\rrepair.post_verify {}/{} {}", current + 1, total, file);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            } else if current > 0 && current % 25 == 0 {
                print!("\rrepair.post_verify {}/{} {}", current, total, file);
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        };
        println!("repair.post_verify=started");
        let post_issues = manager
            .verify_integrity(&api_client, Some(post_progress))
            .await
            .context("Post-repair verify_integrity failed")?;
        println!("\nrepair.post_verify.issues={}", post_issues.len());
        if !post_issues.is_empty() {
            for issue in post_issues.iter().take(20) {
                println!(
                    "repair.post_verify.issue path={} kind={:?} expected_size={} actual_size={:?} expected_md5={} actual_md5={:?}",
                    issue.path,
                    issue.kind,
                    issue.expected_size,
                    issue.actual_size,
                    issue.expected_md5,
                    issue.actual_md5
                );
            }
            anyhow::bail!(
                "Post-repair verify reported {} issue(s)",
                post_issues.len()
            );
        }
    }

    Ok(())
}
