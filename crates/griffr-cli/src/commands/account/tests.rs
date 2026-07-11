use super::operations::*;
use griffr_common::config::ChannelId;
use griffr_common::runtime::copy_dir_recursive;
use std::io::Write;
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
fn local_low_roots_for_hint_cn_prefers_hypergryph() {
    let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
    let roots = local_low_roots_for_hint(&base, "Endfield", Some(ChannelId::HYPERGRYPH)).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], base.join("Hypergryph").join("Endfield"));
}

#[test]
fn local_low_roots_for_hint_global_prefers_gryphline() {
    let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
    let roots =
        local_low_roots_for_hint(&base, "Endfield", Some(ChannelId::GRYPHLINE)).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0], base.join("Gryphline").join("Endfield"));
}

#[test]
fn local_low_roots_for_hint_rejects_invalid_channel_for_game() {
    let base = PathBuf::from("C:\\Users\\Test\\AppData\\LocalLow");
    let channel = ChannelId::new("999").unwrap();
    let err = local_low_roots_for_hint(&base, "FutureGame", Some(channel)).unwrap_err();
    assert!(err.to_string().contains("must provide --sdk-dir"));
}

#[compio::test]
async fn ensure_destination_dir_requires_force_when_existing() {
    let temp = tempfile::tempdir().unwrap();
    let existing = temp.path().join("existing");
    std::fs::create_dir_all(&existing).unwrap();

    let err = ensure_destination_dir(&existing, false).await.unwrap_err();
    assert!(err.to_string().contains("Destination exists"));
}

#[test]
fn copy_dir_recursive_copies_nested_content() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let nested = source.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let file = nested.join("token_cache.bin");
    let mut f = std::fs::File::create(&file).unwrap();
    f.write_all(&[1, 2, 3, 4]).unwrap();

    let target = temp.path().join("target");
    let stats = compio::runtime::Runtime::new()
        .unwrap()
        .block_on(copy_dir_recursive(source.clone(), target.clone()))
        .unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.bytes, 4);
    assert_eq!(
        std::fs::read(target.join("nested").join("token_cache.bin")).unwrap(),
        vec![1, 2, 3, 4]
    );
}
