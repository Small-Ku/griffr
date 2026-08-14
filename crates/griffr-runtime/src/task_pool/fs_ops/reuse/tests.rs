use super::{
    classify_reuse_mode, copy_verified_file_async, create_hardlink, storage_volume_id, ReuseMode,
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

#[test]
fn hardlink_atomically_replaces_the_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("destination.bin");
    std::fs::write(&source, b"verified-before-reuse").unwrap();
    std::fs::write(&destination, b"old-destination").unwrap();

    create_hardlink(&source, &destination).unwrap();

    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"verified-before-reuse"
    );
}

#[test]
fn failed_hardlink_keeps_existing_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("missing-source.bin");
    let destination = temp.path().join("destination.bin");
    std::fs::write(&destination, b"keep-me").unwrap();

    create_hardlink(&source, &destination).unwrap_err();

    assert_eq!(std::fs::read(&destination).unwrap(), b"keep-me");
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
    let expected_md5 = griffr_core::to_hex(&<Md5 as md5::Digest>::digest(payload));

    let proof = copy_verified_file_async(
        &source,
        &destination,
        "destination.bin",
        &crate::ContentHash::from(&expected_md5),
        payload.len() as u64,
    )
    .await
    .unwrap();

    assert_eq!(proof.source(), crate::ArtifactSource::ReuseCopy);
    assert_eq!(proof.logical_path(), "destination.bin");
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

    let error = copy_verified_file_async(
        &source,
        &destination,
        "destination.bin",
        &crate::ContentHash::Md5("00000000000000000000000000000000".to_string()),
        8,
    )
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
