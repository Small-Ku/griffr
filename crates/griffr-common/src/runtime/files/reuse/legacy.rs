use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::api::ApiClient;

pub async fn execute_reuse_plan(
    target_path: &Path,
    plan: &super::models::ReusePlan,
    options: super::models::ReuseOptions,
) -> Result<()> {
    if plan.reusable_files.is_empty() {
        return Ok(());
    }

    if options.dry_run {
        info!("Would create {} hardlinks:", plan.reusable_files.len());
        for file in plan.reusable_files.iter().take(10) {
            info!("  {} <- {}", file.path, file.source_server_id);
        }
        if plan.reusable_files.len() > 10 {
            info!("  ... and {} more", plan.reusable_files.len() - 10);
        }
        return Ok(());
    }

    info!(
        "Creating {} hardlinks for reusable files...",
        plan.reusable_files.len()
    );
    let mut hardlink_tasks = Vec::with_capacity(plan.reusable_files.len());
    let mut path_to_source: rapidhash::RapidHashMap<std::path::PathBuf, std::path::PathBuf> =
        rapidhash::RapidHashMap::default();
    for file in &plan.reusable_files {
        let source_file = file.source_path.join(&file.path);
        let target_file = target_path.join(&file.path);
        path_to_source.insert(target_file.clone(), source_file.clone());
        hardlink_tasks.push(crate::runtime::task_pool::Task::Hardlink {
            src: source_file,
            dest: target_file,
        });
    }

    let mut cfg = crate::runtime::task_pool::TaskPoolConfig::default();
    cfg.io_slots = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let result = crate::runtime::task_pool::run_tasks(hardlink_tasks, cfg)
        .context("Failed to execute hardlink task pool")?;

    let mut hardlink_failures: Vec<(std::path::PathBuf, String)> = Vec::new();
    for event in result.events {
        if let crate::runtime::task_pool::ProgressEvent::Failed { path, reason } = event {
            hardlink_failures.push((std::path::PathBuf::from(path), reason));
        }
    }

    if !hardlink_failures.is_empty() && options.allow_copy_fallback {
        for (target, _reason) in &hardlink_failures {
            if let Some(source) = path_to_source.get(target) {
                if let Some(parent) = target.parent() {
                    compio::fs::create_dir_all(parent).await?;
                }
                if compio::fs::metadata(target).await.is_ok() {
                    let _ = compio::fs::remove_file(target).await;
                }
                std::fs::copy(source, target).with_context(|| {
                    format!(
                        "Copy fallback failed for {} -> {}",
                        source.display(),
                        target.display()
                    )
                })?;
            }
        }
    } else if !hardlink_failures.is_empty() {
        anyhow::bail!(
            "Failed to create hardlinks for {} files. Use --force-copy to allow copying. \
             First failure: {} - {}",
            hardlink_failures.len(),
            hardlink_failures[0].0.display(),
            hardlink_failures[0].1
        );
    }

    Ok(())
}

pub fn print_reuse_plan_summary(plan: &super::models::ReusePlan, force_copy: bool) {
    if !plan.source_servers.is_empty() {
        info!("File reuse plan:");
        info!(" Source servers:");
        for source in &plan.source_servers {
            info!(
                " - {} (version {}, {} files)",
                source.server_id, source.version, source.file_count
            );
        }
        info!(
            " Reusable files: {} ({:.2} GB)",
            plan.reusable_files.len(),
            plan.reusable_size as f64 / 1024.0 / 1024.0 / 1024.0
        );
        info!(
            " Files to download: {} ({:.2} GB)",
            plan.download_files.len(),
            plan.download_size as f64 / 1024.0 / 1024.0 / 1024.0
        );

        if plan.requires_copy_fallback && !force_copy {
            warn!("Some files may require copy fallback (different volumes)");
            info!("Use --force-copy to allow copying if hardlink fails.");
        }
    } else {
        info!("No eligible source servers found for file reuse.");
    }
}

pub async fn download_remaining_files(
    _api_client: &ApiClient,
    download_files: &[super::models::DownloadFile],
    install_path: &Path,
    files_base_url: &str,
) -> Result<()> {
    if download_files.is_empty() {
        return Ok(());
    }

    info!("Downloading remaining {} files...", download_files.len());

    let total_size: u64 = download_files.iter().map(|f| f.size).sum();
    let tasks = download_files
        .iter()
        .map(|file| crate::runtime::task_pool::Task::Download {
            url: format!("{}/{}", files_base_url, file.path),
            dest: install_path.join(&file.path),
            logical_path: file.path.clone(),
            expected_md5: file.md5.clone(),
            expected_size: Some(file.size),
            retry_count: 0,
        })
        .collect::<Vec<_>>();

    let mut pool_cfg = crate::runtime::task_pool::TaskPoolConfig::default();
    pool_cfg.io_slots = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let result = crate::runtime::task_pool::run_tasks(tasks, pool_cfg)
        .context("Task pool failed for reuse downloads")?;

    let mut completed = 0usize;
    let mut downloaded: u64 = 0;
    let mut failures = Vec::new();
    for event in result.events {
        match event {
            crate::runtime::task_pool::ProgressEvent::Verified { path, ok, issue } => {
                completed += 1;
                if download_files.len() <= 10 || completed % 10 == 0 {
                    info!(
                        "  [{}/{}] Downloaded {} ({:.1} MB / {:.1} MB)",
                        completed,
                        download_files.len(),
                        path,
                        downloaded as f64 / 1024.0 / 1024.0,
                        total_size as f64 / 1024.0 / 1024.0
                    );
                }
                if !ok {
                    if let Some(issue) = issue {
                        failures.push(format!("{} ({:?})", issue.path, issue.kind));
                    } else {
                        failures.push(path);
                    }
                }
            }
            crate::runtime::task_pool::ProgressEvent::Downloaded { bytes, .. } => {
                downloaded += bytes;
            }
            crate::runtime::task_pool::ProgressEvent::Failed { path, reason } => {
                failures.push(format!("{} ({})", path, reason));
            }
            _ => {}
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "Failed to download {} file(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    info!(
        "Download complete ({} files, {:.2} MB).",
        download_files.len(),
        total_size as f64 / 1024.0 / 1024.0
    );

    Ok(())
}
