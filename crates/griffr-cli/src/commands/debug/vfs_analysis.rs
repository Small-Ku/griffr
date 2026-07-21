use std::path::PathBuf;

use anyhow::Result;
use griffr_common::api::crypto;

use super::vfs_support::*;
use crate::progress::StepProgress;
use crate::{GlobalOptions, SnapshotHashScope, VfsDiffAgainst};
use griffr_common::runtime::{
    path_is_dir, persistent_path, resource_manifest_filename, streaming_assets_path,
    ResourceManifestKind, RESOURCE_GROUP_INITIAL, RESOURCE_GROUP_MAIN,
};

pub(super) async fn snapshot_root_state(
    root: std::path::PathBuf,
    against: VfsDiffAgainst,
    key: &str,
    hash_check: bool,
    hash_progress_callback: Option<&dyn Fn(usize, usize, &str)>,
) -> ResourceRootSnapshot {
    let root_is_dir = path_is_dir(&root).await;
    if !root_is_dir {
        return ResourceRootSnapshot {
            root_path: root.display().to_string(),
            present: false,
            manifest_counts: ManifestFileCounts {
                index_initial: None,
                index_main: None,
                pref_initial: None,
                pref_main: None,
            },
            scope: None,
            expected_files: 0,
            actual_files: 0,
            missing_files: 0,
            extra_files: 0,
            hash_mismatch_files: 0,
            hash_checked: false,
            actual_paths: Vec::new(),
            missing_paths: Vec::new(),
            extra_paths: Vec::new(),
            hash_mismatches: Vec::new(),
            error: Some(format!("Missing root directory {}", root.display())),
        };
    }

    let manifests = match async {
        Ok::<LocalResManifests, anyhow::Error>(LocalResManifests {
            index_initial: try_read_local_res_index(
                &root.join(resource_manifest_filename(
                    ResourceManifestKind::Index,
                    RESOURCE_GROUP_INITIAL,
                )),
                key,
            )
            .await?,
            index_main: try_read_local_res_index(
                &root.join(resource_manifest_filename(
                    ResourceManifestKind::Index,
                    RESOURCE_GROUP_MAIN,
                )),
                key,
            )
            .await?,
            pref_initial: try_read_local_res_index(
                &root.join(resource_manifest_filename(
                    ResourceManifestKind::Pref,
                    RESOURCE_GROUP_INITIAL,
                )),
                key,
            )
            .await?,
            pref_main: try_read_local_res_index(
                &root.join(resource_manifest_filename(
                    ResourceManifestKind::Pref,
                    RESOURCE_GROUP_MAIN,
                )),
                key,
            )
            .await?,
        })
    }
    .await
    {
        Ok(value) => value,
        Err(err) => {
            return ResourceRootSnapshot {
                root_path: root.display().to_string(),
                present: true,
                manifest_counts: ManifestFileCounts {
                    index_initial: None,
                    index_main: None,
                    pref_initial: None,
                    pref_main: None,
                },
                scope: None,
                expected_files: 0,
                actual_files: 0,
                missing_files: 0,
                extra_files: 0,
                hash_mismatch_files: 0,
                hash_checked: false,
                actual_paths: Vec::new(),
                missing_paths: Vec::new(),
                extra_paths: Vec::new(),
                hash_mismatches: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let manifest_counts = manifest_file_counts(&manifests);
    let expected = match select_expected_vfs_set(against, &manifests) {
        Ok(v) => v,
        Err(err) => {
            return ResourceRootSnapshot {
                root_path: root.display().to_string(),
                present: true,
                manifest_counts,
                scope: None,
                expected_files: 0,
                actual_files: 0,
                missing_files: 0,
                extra_files: 0,
                hash_mismatch_files: 0,
                hash_checked: false,
                actual_paths: Vec::new(),
                missing_paths: Vec::new(),
                extra_paths: Vec::new(),
                hash_mismatches: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };
    let expected_with_checksums = select_expected_vfs_map(against, &manifests).ok();
    let actual = match collect_actual_vfs_files(&root).await {
        Ok(v) => v,
        Err(err) => {
            return ResourceRootSnapshot {
                root_path: root.display().to_string(),
                present: true,
                manifest_counts,
                scope: Some(expected.scope.as_str().to_string()),
                expected_files: expected.entries.len(),
                actual_files: 0,
                missing_files: expected.entries.len(),
                extra_files: 0,
                hash_mismatch_files: 0,
                hash_checked: false,
                actual_paths: Vec::new(),
                missing_paths: expected.entries.into_iter().collect(),
                extra_paths: Vec::new(),
                hash_mismatches: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let missing = expected
        .entries
        .difference(&actual)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let extra = actual
        .difference(&expected.entries)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let hash_mismatches = if hash_check {
        match expected_with_checksums.as_ref() {
            Some(map) => collect_hash_mismatches(&root, &map.entries, hash_progress_callback).await,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    ResourceRootSnapshot {
        root_path: root.display().to_string(),
        present: true,
        manifest_counts,
        scope: Some(expected.scope.as_str().to_string()),
        expected_files: expected.entries.len(),
        actual_files: actual.len(),
        missing_files: missing.len(),
        extra_files: extra.len(),
        hash_mismatch_files: hash_mismatches.len(),
        hash_checked: hash_check,
        actual_paths: actual.into_iter().collect(),
        missing_paths: missing.into_iter().collect(),
        extra_paths: extra.into_iter().collect(),
        hash_mismatches,
        error: None,
    }
}

pub(super) fn print_sample(label: &str, set: &std::collections::BTreeSet<String>, limit: usize) {
    println!("{}={}", label, set.len());
    if limit == 0 || set.is_empty() {
        return;
    }
    let mut shown = 0usize;
    for item in set {
        println!("{}: {}", label, item);
        shown += 1;
        if shown >= limit {
            break;
        }
    }
    if set.len() > shown {
        println!("{}: ... ({} more)", label, set.len() - shown);
    }
}

pub async fn vfs_diff(
    path: PathBuf,
    against: VfsDiffAgainst,
    key: Option<String>,
    show_limit: usize,
    _opts: GlobalOptions,
) -> Result<()> {
    let root = resolve_vfs_root(&path).await?;
    let key = key.unwrap_or_else(|| crypto::RES_INDEX_KEY.to_string());

    let manifests = LocalResManifests {
        index_initial: try_read_local_res_index(
            &root.join(resource_manifest_filename(
                ResourceManifestKind::Index,
                RESOURCE_GROUP_INITIAL,
            )),
            &key,
        )
        .await?,
        index_main: try_read_local_res_index(
            &root.join(resource_manifest_filename(
                ResourceManifestKind::Index,
                RESOURCE_GROUP_MAIN,
            )),
            &key,
        )
        .await?,
        pref_initial: try_read_local_res_index(
            &root.join(resource_manifest_filename(
                ResourceManifestKind::Pref,
                RESOURCE_GROUP_INITIAL,
            )),
            &key,
        )
        .await?,
        pref_main: try_read_local_res_index(
            &root.join(resource_manifest_filename(
                ResourceManifestKind::Pref,
                RESOURCE_GROUP_MAIN,
            )),
            &key,
        )
        .await?,
    };
    let expected = select_expected_vfs_set(against, &manifests)?;
    let actual = collect_actual_vfs_files(&root).await?;

    let missing = expected
        .entries
        .difference(&actual)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let extra = actual
        .difference(&expected.entries)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();

    println!("root={}", root.display());
    println!(
        "against={}",
        match against {
            VfsDiffAgainst::Persistent => "persistent",
            VfsDiffAgainst::Streamingassets => "streamingassets",
        }
    );
    println!("scope={}", expected.scope);
    println!(
        "manifests=index_initial:{} index_main:{} pref_initial:{} pref_main:{}",
        manifests.index_initial.is_some(),
        manifests.index_main.is_some(),
        manifests.pref_initial.is_some(),
        manifests.pref_main.is_some()
    );
    println!("expected_files={}", expected.entries.len());
    println!("actual_files={}", actual.len());
    println!("missing_files={}", missing.len());
    println!("extra_files={}", extra.len());

    print_sample("missing", &missing, show_limit);
    print_sample("extra", &extra, show_limit);

    Ok(())
}

pub async fn snapshot_resource_state(
    path: PathBuf,
    output: Option<PathBuf>,
    hash_check: SnapshotHashScope,
    opts: GlobalOptions,
) -> Result<()> {
    let source_path = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()?.join(path.clone())
    };
    let endfield_data_root = resolve_endfield_data_root(&source_path).await?;
    let key = crypto::RES_INDEX_KEY;

    let persistent_hash_check = matches!(
        hash_check,
        SnapshotHashScope::Persistent | SnapshotHashScope::All
    );
    let persistent_progress = StepProgress::new("snapshot.persistent-hash", opts.verbose);
    let persistent_progress_cb = |completed, total, file: &str| {
        persistent_progress.update_count(completed, total, file);
    };
    let persistent_hash_progress: Option<&dyn Fn(usize, usize, &str)> = if persistent_hash_check {
        Some(&persistent_progress_cb)
    } else {
        None
    };
    let persistent = snapshot_root_state(
        persistent_path(&endfield_data_root),
        VfsDiffAgainst::Persistent,
        key,
        persistent_hash_check,
        persistent_hash_progress,
    )
    .await;
    persistent_progress.finish();

    let streaming_hash_check = matches!(hash_check, SnapshotHashScope::All);
    let streaming_progress = StepProgress::new("snapshot.streamingassets-hash", opts.verbose);
    let streaming_progress_cb = |completed, total, file: &str| {
        streaming_progress.update_count(completed, total, file);
    };
    let streaming_hash_progress: Option<&dyn Fn(usize, usize, &str)> = if streaming_hash_check {
        Some(&streaming_progress_cb)
    } else {
        None
    };
    let streamingassets = snapshot_root_state(
        streaming_assets_path(&endfield_data_root),
        VfsDiffAgainst::Streamingassets,
        key,
        streaming_hash_check,
        streaming_hash_progress,
    )
    .await;
    streaming_progress.finish();

    let snapshot = ResourceStateSnapshot {
        schema_version: 1,
        captured_at_utc: chrono::Utc::now().to_rfc3339(),
        source_path: source_path.display().to_string(),
        endfield_data_path: endfield_data_root.display().to_string(),
        persistent,
        streamingassets,
    };
    let payload = serde_json::to_value(&snapshot)?;
    emit_json(output, payload).await?;
    Ok(())
}
