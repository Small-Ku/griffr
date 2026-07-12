use crate::error::{Error, Result};
use rapidhash::{RapidHashMap as HashMap, RapidHashSet as HashSet};

use crate::api::types::GameFileEntry;
use crate::api::ApiClient;
use crate::config::{ChannelPair, GameId};
pub(crate) use crate::runtime::is_launcher_metadata_path;
use crate::runtime::task_pool::{run_tasks, ProgressEvent, Task, TaskPoolConfig};

#[allow(clippy::too_many_arguments)]
pub async fn plan_file_reuse(
    game_id: GameId,
    _target_channel_id: ChannelPair,
    _target_version: &str,
    target_manifest: &[GameFileEntry],
    source_installs: &[super::models::SourceInstallInput],
    api_client: &ApiClient,
) -> Result<super::models::ReusePlan> {
    let mut source_channels: Vec<super::models::SourceChannel> = Vec::new();
    let mut source_manifests: Vec<Vec<GameFileEntry>> = Vec::new();

    for source in source_installs {
        let channel_id = source.channel_id.clone();
        let version = &source.version;

        let preset = match crate::config::KnownTargets::resolve(&game_id, &channel_id) {
            Some(p) => p,
            None => continue,
        };

        let version_info = match api_client
            .get_latest_game(&preset.target, Some(version))
            .await
        {
            Ok(info) => info,
            Err(_) => continue,
        };

        let pkg = match &version_info.pkg {
            Some(pkg) => pkg,
            None => continue,
        };

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

        source_channels.push(super::models::SourceChannel {
            channel_id,
            version: version.clone(),
            install_path: source.install_path.clone(),
            file_count: manifest.len(),
        });
        source_manifests.push(manifest);
    }

    if source_channels.is_empty() {
        return Ok(super::models::ReusePlan {
            source_channels: vec![],
            reusable_files: vec![],
            download_files: target_manifest
                .iter()
                .map(|e| super::models::DownloadFile {
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

    let target_manifest_map: HashMap<&str, &GameFileEntry> = target_manifest
        .iter()
        .filter(|e| !is_launcher_metadata_path(&e.path))
        .map(|e| (e.path.as_str(), e))
        .collect();

    let mut reusable_files: Vec<super::models::ReusableFile> = Vec::new();
    let mut reusable_paths: HashSet<String> = HashSet::default();
    let mut reusable_size: u64 = 0;

    for (idx, source) in source_channels.iter().enumerate() {
        let source_manifest = match source_manifests.get(idx) {
            Some(m) => m,
            None => continue,
        };
        let mut candidate_meta: HashMap<String, (String, u64)> = HashMap::default();
        let mut verify_tasks = Vec::new();

        for entry in source_manifest {
            if is_launcher_metadata_path(&entry.path) {
                continue;
            }
            if reusable_paths.contains(&entry.path) {
                continue;
            }

            if let Some(target_entry) = target_manifest_map.get(entry.path.as_str()) {
                if target_entry.md5.to_lowercase() == entry.md5.to_lowercase()
                    && target_entry.size == entry.size
                {
                    let source_file = source.install_path.join(&entry.path);
                    candidate_meta.insert(entry.path.clone(), (entry.md5.clone(), entry.size));
                    verify_tasks.push(Task::Verify {
                        path: source_file,
                        logical_path: entry.path.clone(),
                        expected_md5: entry.md5.clone(),
                        expected_size: Some(entry.size),
                        on_fail: None,
                    });
                }
            }
        }

        if verify_tasks.is_empty() {
            continue;
        }

        let cfg = TaskPoolConfig::default();
        let verify_result = run_tasks(verify_tasks, cfg)?;
        for event in verify_result.events {
            if let ProgressEvent::Verified { path, ok, .. } = event {
                if !ok || reusable_paths.contains(&path) {
                    continue;
                }
                if let Some((md5, size)) = candidate_meta.get(&path) {
                    reusable_files.push(super::models::ReusableFile {
                        path: path.clone(),
                        md5: md5.clone(),
                        size: *size,
                        source_channel_id: source.channel_id.clone(),
                        source_path: source.install_path.clone(),
                    });
                    reusable_paths.insert(path);
                    reusable_size += *size;
                }
            }
        }
    }

    let mut download_files: Vec<super::models::DownloadFile> = Vec::new();
    let mut download_size: u64 = 0;

    for entry in target_manifest {
        if is_launcher_metadata_path(&entry.path) {
            continue;
        }
        if !reusable_paths.contains(&entry.path) {
            download_files.push(super::models::DownloadFile {
                path: entry.path.clone(),
                md5: entry.md5.clone(),
                size: entry.size,
            });
            download_size += entry.size;
        }
    }

    let requires_copy_fallback = false;

    Ok(super::models::ReusePlan {
        source_channels,
        reusable_files,
        download_files,
        reusable_size,
        download_size,
        requires_copy_fallback,
    })
}

pub fn derive_files_base_url(file_path: &str) -> Result<String> {
    let normalized = file_path.trim_end_matches('/');
    if let Some(base) = normalized.strip_suffix("/game_files") {
        return Ok(base.to_string());
    }
    if normalized.ends_with("/files") {
        return Ok(normalized.to_string());
    }
    Err(Error::Config(format!(
        "Expected file_path to end with '/game_files' or '/files', got: {}",
        file_path
    )))
}
