use super::utils::{
    select_expected_vfs_map, select_expected_vfs_set, sorted_difference, LocalResManifests,
    VfsExpectedScope,
};
use crate::VfsDiffAgainst;
use griffr_common::api::types::ResIndex;
use griffr_common::api::types::ResIndexFile;

fn make_index(paths: &[&str]) -> ResIndex {
    ResIndex {
        version: "test".to_string(),
        path: String::new(),
        files: paths
            .iter()
            .enumerate()
            .map(|(i, p)| ResIndexFile {
                index: i as u64,
                name: (*p).to_string(),
                hash: None,
                size: 1,
                r#type: 0,
                md5: Some(format!("{:032x}", i + 1)),
                manifest: 0,
            })
            .collect(),
    }
}

#[test]
fn persistent_pref_only_when_pref_exists() {
    let manifests = LocalResManifests {
        index_initial: Some(make_index(&["VFS/A/a.chk"])),
        index_main: Some(make_index(&["VFS/B/b.chk"])),
        pref_initial: Some(make_index(&["VFS/P/p.chk"])),
        pref_main: None,
    };
    let selected = select_expected_vfs_set(VfsDiffAgainst::Persistent, &manifests).unwrap();
    assert_eq!(selected.scope, VfsExpectedScope::PrefOnly);
    assert_eq!(selected.entries.len(), 1);
    assert!(selected.entries.contains("vfs/p/p.chk"));
}

#[test]
fn persistent_falls_back_to_index_when_no_pref() {
    let manifests = LocalResManifests {
        index_initial: Some(make_index(&["VFS/A/a.chk"])),
        index_main: Some(make_index(&["VFS/B/b.chk"])),
        pref_initial: None,
        pref_main: None,
    };
    let selected = select_expected_vfs_set(VfsDiffAgainst::Persistent, &manifests).unwrap();
    assert_eq!(selected.scope, VfsExpectedScope::IndexFullFallback);
    assert_eq!(selected.entries.len(), 2);
    assert!(selected.entries.contains("vfs/a/a.chk"));
    assert!(selected.entries.contains("vfs/b/b.chk"));
}

#[test]
fn streamingassets_uses_index_full_even_if_pref_exists() {
    let manifests = LocalResManifests {
        index_initial: Some(make_index(&["VFS/A/a.chk"])),
        index_main: Some(make_index(&["VFS/B/b.chk"])),
        pref_initial: Some(make_index(&["VFS/P/p.chk"])),
        pref_main: Some(make_index(&["VFS/Q/q.chk"])),
    };
    let selected = select_expected_vfs_set(VfsDiffAgainst::Streamingassets, &manifests).unwrap();
    assert_eq!(selected.scope, VfsExpectedScope::IndexFull);
    assert_eq!(selected.entries.len(), 2);
    assert!(selected.entries.contains("vfs/a/a.chk"));
    assert!(selected.entries.contains("vfs/b/b.chk"));
}

#[test]
fn expected_vfs_map_uses_pref_checksums_for_persistent() {
    let manifests = LocalResManifests {
        index_initial: Some(make_index(&["VFS/A/a.chk"])),
        index_main: None,
        pref_initial: Some(make_index(&["VFS/P/p.chk"])),
        pref_main: None,
    };
    let selected = select_expected_vfs_map(VfsDiffAgainst::Persistent, &manifests).unwrap();
    assert_eq!(selected.scope, VfsExpectedScope::PrefOnly);
    assert_eq!(selected.entries.len(), 1);
    assert!(selected.entries.contains_key("vfs/p/p.chk"));
}

#[test]
fn sorted_difference_returns_only_left_unique_values() {
    let left = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let right = vec!["b".to_string(), "d".to_string()];
    let diff = sorted_difference(&left, &right);
    assert_eq!(diff.len(), 2);
    assert!(diff.contains("a"));
    assert!(diff.contains("c"));
}
