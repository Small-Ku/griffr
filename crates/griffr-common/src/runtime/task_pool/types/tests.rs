use super::{FileEnsureTask, Task, TaskOutcome, TransferClass, WorkerEvent};
use std::path::PathBuf;

#[test]
fn transient_worker_progress_is_not_retained_as_an_outcome() {
    assert!(WorkerEvent::DownloadedBytes {
        path: "asset.bin".to_string(),
        bytes: 64,
        total_bytes: 128,
    }
    .into_outcome()
    .is_none());
    assert!(WorkerEvent::PatchProgress {
        path: "patch.json".to_string(),
        completed: 1,
        total: 2,
    }
    .into_outcome()
    .is_none());
}

#[test]
fn normal_file_ensure_starts_with_cpu_verification() {
    let task = Task::ensure_file(FileEnsureTask {
        dest: PathBuf::from("game/file.bin"),
        logical_path: "file.bin".to_string(),
        expected_md5: "00".repeat(16),
        expected_size: 4,
        source_candidates: vec![PathBuf::from("reuse/file.bin")],
        download_url: Some("https://example.invalid/file.bin".to_string()),
        allow_copy_fallback: true,
        prefer_reuse: false,
        retry_count: 0,
        transfer_class: TransferClass::General,
    });

    assert!(matches!(
        task,
        Task::Verify {
            on_fail: Some(repair),
            ..
        } if matches!(*repair, Task::RepairFile { .. })
    ));
}

#[test]
fn explicit_relink_skips_target_verification() {
    let task = Task::ensure_file(FileEnsureTask {
        dest: PathBuf::from("game/file.bin"),
        logical_path: "file.bin".to_string(),
        expected_md5: "00".repeat(16),
        expected_size: 4,
        source_candidates: vec![PathBuf::from("reuse/file.bin")],
        download_url: None,
        allow_copy_fallback: false,
        prefer_reuse: true,
        retry_count: 0,
        transfer_class: TransferClass::Vfs,
    });

    assert!(matches!(
        task,
        Task::RepairFile {
            transfer_class: TransferClass::Vfs,
            ..
        }
    ));
}

#[test]
fn changed_worker_facts_become_task_outcomes() {
    assert!(matches!(
        WorkerEvent::Changed {
            path: "data/file.bin".to_string(),
        }
        .into_outcome(),
        Some(TaskOutcome::Changed { path }) if path == "data/file.bin"
    ));
}

#[test]
fn durable_worker_facts_become_task_outcomes() {
    assert!(matches!(
        WorkerEvent::Downloaded {
            path: "asset.bin".to_string(),
            bytes: 128,
        }
        .into_outcome(),
        Some(TaskOutcome::Downloaded { path, bytes })
            if path == "asset.bin" && bytes == 128
    ));
}

#[test]
fn reuse_group_claims_first_verified_source_immediately() {
    let source = PathBuf::from("reuse/source.bin");
    let other = PathBuf::from("reuse/other.bin");
    let group = super::ReuseCandidateGroup::new(
        2,
        vec![vec![PathBuf::from("copy/source.bin")]],
        vec![source.clone(), other],
        PathBuf::from("game/file.bin"),
        "file.bin".to_string(),
        "00".repeat(16),
        4,
        None,
        true,
        0,
        TransferClass::General,
    );
    let (event_tx, _event_rx) = flume::unbounded();
    let mut spawned = Vec::new();

    group.finish_volume(false, Some(source.clone()), &mut spawned, &event_tx);
    assert!(matches!(
        spawned.as_slice(),
        [Task::ReuseFile { source: winner, copy_only: false, .. }] if winner == &source
    ));

    group.finish_volume(false, None, &mut spawned, &event_tx);
    assert_eq!(spawned.len(), 1, "late probes must not replace the winner");
}

#[test]
fn reuse_group_defers_cross_volume_copy_until_hardlink_probes_fail() {
    let copy_source = PathBuf::from("copy/source.bin");
    let group = super::ReuseCandidateGroup::new(
        2,
        vec![vec![copy_source.clone()]],
        vec![copy_source],
        PathBuf::from("game/file.bin"),
        "file.bin".to_string(),
        "00".repeat(16),
        4,
        None,
        true,
        0,
        TransferClass::General,
    );
    let (event_tx, _event_rx) = flume::unbounded();
    let mut spawned = Vec::new();

    group.finish_volume(false, None, &mut spawned, &event_tx);
    assert!(spawned.is_empty());
    group.finish_volume(false, None, &mut spawned, &event_tx);
    assert!(matches!(
        spawned.as_slice(),
        [Task::VerifyReuseVolume { copy_only: true, .. }]
    ));
}
