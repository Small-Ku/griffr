use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::crypto;

use super::utils::*;
use crate::{GlobalOptions, SnapshotHashScope, VfsDiffAgainst};
use griffr_common::runtime::{
    persistent_path, resource_manifest_filename, streaming_assets_path, ResourceManifestKind,
    RESOURCE_GROUP_INITIAL, RESOURCE_GROUP_MAIN,
};

pub(super) async fn snapshot_root_state(
    root: std::path::PathBuf,
    against: VfsDiffAgainst,
    key: &str,
    hash_check: bool,
) -> ResourceRootSnapshot {
    if !root.is_dir() {
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
    let actual = match collect_actual_vfs_files(&root) {
        Ok(v) => v,
        Err(err) => {
            return ResourceRootSnapshot {
                root_path: root.display().to_string(),
                present: true,
                manifest_counts,
                scope: Some(expected.scope.to_string()),
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
        expected_with_checksums
            .as_ref()
            .map(|map| collect_hash_mismatches(&root, &map.entries))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    ResourceRootSnapshot {
        root_path: root.display().to_string(),
        present: true,
        manifest_counts,
        scope: Some(expected.scope.to_string()),
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
    let root = resolve_vfs_root(&path)?;
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
    let actual = collect_actual_vfs_files(&root)?;

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
    _opts: GlobalOptions,
) -> Result<()> {
    let source_path = if path.is_absolute() {
        path.clone()
    } else {
        std::env::current_dir()?.join(path.clone())
    };
    let endfield_data_root = resolve_endfield_data_root(&source_path)?;
    let key = crypto::RES_INDEX_KEY;

    let persistent = snapshot_root_state(
        persistent_path(&endfield_data_root),
        VfsDiffAgainst::Persistent,
        key,
        matches!(
            hash_check,
            SnapshotHashScope::Persistent | SnapshotHashScope::All
        ),
    )
    .await;
    let streamingassets = snapshot_root_state(
        streaming_assets_path(&endfield_data_root),
        VfsDiffAgainst::Streamingassets,
        key,
        matches!(hash_check, SnapshotHashScope::All),
    )
    .await;

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

pub(super) fn print_changed_sample(
    label: &str,
    set: &std::collections::BTreeSet<String>,
    limit: usize,
) {
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

pub(super) fn diff_root_snapshots(
    name: &str,
    before: &ResourceRootSnapshot,
    after: &ResourceRootSnapshot,
    show_limit: usize,
) {
    println!("[{}]", name);
    println!(
        "scope={} -> {}",
        before.scope.as_deref().unwrap_or("<none>"),
        after.scope.as_deref().unwrap_or("<none>")
    );
    println!(
        "expected_files={} -> {}",
        before.expected_files, after.expected_files
    );
    println!(
        "actual_files={} -> {}",
        before.actual_files, after.actual_files
    );
    println!(
        "missing_files={} -> {}",
        before.missing_files, after.missing_files
    );
    println!(
        "extra_files={} -> {}",
        before.extra_files, after.extra_files
    );
    println!(
        "hash_mismatch_files={} -> {}",
        before.hash_mismatch_files, after.hash_mismatch_files
    );
    println!(
        "hash_checked={} -> {}",
        before.hash_checked, after.hash_checked
    );
    if let Some(err) = &before.error {
        println!("before_error={}", err);
    }
    if let Some(err) = &after.error {
        println!("after_error={}", err);
    }

    let actual_added = sorted_difference(&after.actual_paths, &before.actual_paths);
    let actual_removed = sorted_difference(&before.actual_paths, &after.actual_paths);
    let now_missing = sorted_difference(&after.missing_paths, &before.missing_paths);
    let resolved_missing = sorted_difference(&before.missing_paths, &after.missing_paths);

    let before_mismatch_paths = before
        .hash_mismatches
        .iter()
        .map(|m| m.path.clone())
        .collect::<Vec<_>>();
    let after_mismatch_paths = after
        .hash_mismatches
        .iter()
        .map(|m| m.path.clone())
        .collect::<Vec<_>>();
    let new_mismatches = sorted_difference(&after_mismatch_paths, &before_mismatch_paths);
    let resolved_mismatches = sorted_difference(&before_mismatch_paths, &after_mismatch_paths);

    print_changed_sample("actual_added", &actual_added, show_limit);
    print_changed_sample("actual_removed", &actual_removed, show_limit);
    print_changed_sample("missing_added", &now_missing, show_limit);
    print_changed_sample("missing_resolved", &resolved_missing, show_limit);
    print_changed_sample("mismatch_added", &new_mismatches, show_limit);
    print_changed_sample("mismatch_resolved", &resolved_mismatches, show_limit);
}

pub async fn diff_resource_snapshots(
    before: PathBuf,
    after: PathBuf,
    show_limit: usize,
    _opts: GlobalOptions,
) -> Result<()> {
    let before_body = compio::fs::read(&before)
        .await
        .with_context(|| format!("Failed to read {}", before.display()))?;
    let after_body = compio::fs::read(&after)
        .await
        .with_context(|| format!("Failed to read {}", after.display()))?;

    let before_snapshot: ResourceStateSnapshot = serde_json::from_slice(&before_body)
        .with_context(|| format!("Failed to parse {}", before.display()))?;
    let after_snapshot: ResourceStateSnapshot = serde_json::from_slice(&after_body)
        .with_context(|| format!("Failed to parse {}", after.display()))?;

    println!("before={}", before.display());
    println!("after={}", after.display());
    println!(
        "captured_at={} -> {}",
        before_snapshot.captured_at_utc, after_snapshot.captured_at_utc
    );
    println!(
        "endfield_data={} -> {}",
        before_snapshot.endfield_data_path, after_snapshot.endfield_data_path
    );

    diff_root_snapshots(
        "persistent",
        &before_snapshot.persistent,
        &after_snapshot.persistent,
        show_limit,
    );
    diff_root_snapshots(
        "streamingassets",
        &before_snapshot.streamingassets,
        &after_snapshot.streamingassets,
        show_limit,
    );

    Ok(())
}
