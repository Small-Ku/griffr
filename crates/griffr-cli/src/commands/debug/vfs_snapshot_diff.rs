use std::path::PathBuf;

use anyhow::{Context, Result};

use super::vfs_support::*;
use crate::GlobalOptions;

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
