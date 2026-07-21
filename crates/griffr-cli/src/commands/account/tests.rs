use super::operations::*;
use griffr_common::config::RegionId;
use griffr_common::runtime::copy_dir_recursive;
use std::path::PathBuf;

#[test]
fn select_latest_sdk_dir_prefers_newest_mtime() {
    let temp = tempfile::tempdir().unwrap();
    let older = temp.path().join("sdk_data_old");
    let newer = temp.path().join("sdk_data_new");
    std::fs::create_dir_all(&older).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    std::fs::create_dir_all(&newer).unwrap();

    let selected = select_latest_sdk_dir(temp.path()).unwrap();
    assert_eq!(selected, newer);
}

#[test]
fn select_latest_sdk_dir_from_roots_prefers_newest_across_roots() {
    let temp = tempfile::tempdir().unwrap();
    let root_a = temp.path().join("Hypergryph").join("Endfield");
    let root_b = temp.path().join("Gryphline").join("Endfield");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    let older = root_a.join("sdk_data_older");
    let newer = root_b.join("sdk_data_newer");
    std::fs::create_dir_all(&older).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    std::fs::create_dir_all(&newer).unwrap();

    let selected = select_latest_sdk_dir_from_roots(&[root_a.clone(), root_b.clone()]).unwrap();
    assert_eq!(selected, newer);
}

#[test]
fn local_low_roots_for_cn_prefers_hypergryph() {
    let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
    let roots = local_low_roots_for_hint(&base, "Endfield", Some(RegionId::Cn)).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], base.join("Hypergryph").join("Endfield"));
}

#[test]
fn local_low_roots_for_sg_prefers_gryphline() {
    let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
    let roots = local_low_roots_for_hint(&base, "Endfield", Some(RegionId::Sg)).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], base.join("Gryphline").join("Endfield"));
}

#[compio::test]
async fn ensure_destination_dir_requires_force_when_existing() {
    let temp = tempfile::tempdir().unwrap();
    let existing = temp.path().join("existing");
    compio::fs::create_dir_all(&existing).await.unwrap();

    let err = ensure_destination_dir(&existing, false).await.unwrap_err();
    assert!(err.to_string().contains("Destination exists"));
}

#[compio::test]
async fn copy_dir_recursive_copies_nested_content() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let nested = source.join("nested");
    compio::fs::create_dir_all(&nested).await.unwrap();
    let file = nested.join("token_cache.bin");
    compio::fs::write(&file, vec![1, 2, 3, 4]).await.0.unwrap();

    let target = temp.path().join("target");
    let stats = copy_dir_recursive(source.clone(), target.clone())
        .await
        .unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.bytes, 4);
    assert_eq!(
        compio::fs::read(target.join("nested").join("token_cache.bin"))
            .await
            .unwrap(),
        vec![1, 2, 3, 4]
    );
}
