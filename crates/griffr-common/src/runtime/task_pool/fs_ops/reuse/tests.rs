use super::{
    classify_reuse_mode, copy_verified_file_async, create_hardlink_async, storage_volume_id,
    ReuseMethod, ReuseMode,
};
use md5::Md5;

#[test]
fn volume_classification_only_forces_copy_for_proven_differences() {
    assert_eq!(
        classify_reuse_mode(Some("volume-a"), Some("volume-a")),
        ReuseMode::HardlinkPreferred
    );
    assert_eq!(
        classify_reuse_mode(Some("volume-a"), Some("volume-b")),
        ReuseMode::CopyOnly
    );
    assert_eq!(
        classify_reuse_mode(None, Some("volume-b")),
        ReuseMode::HardlinkPreferred
    );
}

#[compio::test]
async fn hardlink_reuses_the_already_verified_inode_without_rehashing() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("destination.bin");
    compio::fs::write(&source, b"verified-before-reuse".to_vec())
        .await
        .0
        .unwrap();

    create_hardlink_async(&source, &destination).await.unwrap();

    assert_eq!(
        compio::fs::read(&destination).await.unwrap(),
        b"verified-before-reuse"
    );
}

#[compio::test]
async fn failed_hardlink_keeps_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("missing-source.bin");
    let destination = temp.path().join("destination.bin");
    compio::fs::write(&destination, b"keep-me".to_vec())
        .await
        .0
        .unwrap();

    create_hardlink_async(&source, &destination)
        .await
        .unwrap_err();

    assert_eq!(compio::fs::read(&destination).await.unwrap(), b"keep-me");
}

#[compio::test]
async fn async_copy_hashes_while_writing_and_commits_verified_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("destination.bin");
    let payload = b"copy-and-hash-in-one-pass";
    compio::fs::write(&source, payload.to_vec())
        .await
        .0
        .unwrap();
    compio::fs::write(&destination, b"old".to_vec())
        .await
        .0
        .unwrap();
    let expected_md5 = crate::to_hex(&<Md5 as md5::Digest>::digest(payload));

    let method =
        copy_verified_file_async(&source, &destination, &expected_md5, payload.len() as u64)
            .await
            .unwrap();

    assert_eq!(method, ReuseMethod::Copy);
    assert_eq!(compio::fs::read(&destination).await.unwrap(), payload);
}

#[compio::test]
async fn async_copy_mismatch_keeps_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("destination.bin");
    compio::fs::write(&source, b"new-data".to_vec())
        .await
        .0
        .unwrap();
    compio::fs::write(&destination, b"old-data".to_vec())
        .await
        .0
        .unwrap();

    let error =
        copy_verified_file_async(&source, &destination, "00000000000000000000000000000000", 8)
            .await
            .unwrap_err();

    assert!(error.to_string().contains("Copy verification failed"));
    assert_eq!(compio::fs::read(&destination).await.unwrap(), b"old-data");
}

#[compio::test]
async fn volume_identity_is_stable_within_one_temp_directory() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("nested").join("destination.bin");
    compio::fs::write(&source, b"source".to_vec())
        .await
        .0
        .unwrap();

    assert_eq!(
        storage_volume_id(&source),
        storage_volume_id(&destination),
        "missing destination paths should resolve through their existing ancestor"
    );
}
