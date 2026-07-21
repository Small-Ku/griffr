use super::{
    ArchiveRetention, ArchiveWork, FileEnsureTask, PreparedArchive, Task, TaskOutcome,
    TaskPoolConfig, TransferClass, WorkerEvent,
};
use crate::download::extractor::MultiVolumeLayout;
use std::path::PathBuf;

#[test]
fn transient_worker_progress_is_not_a_durable_outcome() {
    assert!(matches!(
        WorkerEvent::progress(
            crate::runtime::ProgressPhase::Download,
            "asset.bin".to_string(),
            64,
            128,
            false,
        ),
        WorkerEvent::Progress { .. }
    ));
    assert!(matches!(
        WorkerEvent::progress(
            crate::runtime::ProgressPhase::Patch,
            "patch.json".to_string(),
            1,
            2,
            false,
        ),
        WorkerEvent::Progress { .. }
    ));
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
fn changed_worker_facts_are_direct_task_outcomes() {
    assert!(matches!(
        WorkerEvent::changed("data/file.bin".to_string()),
        WorkerEvent::Outcome(TaskOutcome::Changed { path }) if path == "data/file.bin"
    ));
}

#[test]
fn durable_worker_facts_are_direct_task_outcomes() {
    assert!(matches!(
        WorkerEvent::downloaded("asset.bin".to_string(), 128),
        WorkerEvent::Outcome(TaskOutcome::Downloaded { path, bytes })
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
        false,
        0,
        TransferClass::General,
    );
    let tasks = group.finish_volume(false, Some(source.clone())).unwrap();
    assert!(matches!(
        tasks.as_slice(),
        [Task::ReuseFile { source: winner, copy_only: false, .. }] if winner == &source
    ));

    let late = group.finish_volume(false, None).unwrap();
    assert!(late.is_empty(), "late probes must not replace the winner");
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
        false,
        0,
        TransferClass::General,
    );
    let first = group.finish_volume(false, None).unwrap();
    assert!(first.is_empty());
    let tasks = group.finish_volume(false, None).unwrap();
    assert!(matches!(
        tasks.as_slice(),
        [Task::VerifyReuseVolume {
            copy_only: true,
            ..
        }]
    ));
}

#[test]
fn archive_shard_state_waits_for_active_shards_before_cleanup() {
    let state = super::ArchiveShardRunState::new();
    state.try_begin().unwrap();
    state.try_begin().unwrap();

    let (report_failure, cleanup_staging) = state.finish(false);
    assert!(report_failure);
    assert!(!cleanup_staging);
    assert!(state.is_failed());

    let (report_failure, cleanup_staging) = state.finish(true);
    assert!(!report_failure);
    assert!(cleanup_staging);
}

#[test]
fn archive_shard_state_rejects_new_work_after_failure() {
    let state = super::ArchiveShardRunState::new();
    state.try_begin().unwrap();
    assert_eq!(state.finish(false), (true, true));
    assert_eq!(state.try_begin(), Err(true));
}

#[test]
fn archive_work_drop_removes_abandoned_staging() {
    let temp = tempfile::tempdir().unwrap();
    let volume = temp.path().join("archive.zip.001");
    std::fs::write(&volume, b"volume").unwrap();
    let staging = temp.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("partial.bin"), b"partial").unwrap();

    let layout = MultiVolumeLayout::from_expected(vec![(volume, 6)]).unwrap();
    let work = ArchiveWork::new(
        "archive".to_string(),
        layout,
        vec![None],
        temp.path().join("install"),
        ArchiveRetention::KeepFullVolumes,
        Vec::new(),
        None,
        crate::runtime::PatchApplyOptions::default(),
        std::sync::Arc::new(std::collections::BTreeMap::new()),
        std::sync::Arc::new(std::collections::BTreeSet::new()),
    )
    .unwrap();
    *work.prepared.lock().unwrap() = Some(PreparedArchive {
        staging_dir: staging.clone(),
        patch_plan: None,
    });

    drop(work);
    assert!(!staging.exists());
}

#[test]
fn retained_remote_archive_requires_one_descriptor_per_volume() {
    let temp = tempfile::tempdir().unwrap();
    let layout = MultiVolumeLayout::from_remote(
        vec![(
            temp.path().join("archive.zip.001"),
            "https://example.invalid/archive.zip.001".to_string(),
            6,
        )],
        temp.path().join("cache"),
    )
    .unwrap();

    let error = ArchiveWork::new(
        "archive".to_string(),
        layout,
        vec![None],
        temp.path().join("install"),
        ArchiveRetention::KeepFullVolumes,
        Vec::new(),
        None,
        crate::runtime::PatchApplyOptions::default(),
        std::sync::Arc::new(std::collections::BTreeMap::new()),
        std::sync::Arc::new(std::collections::BTreeSet::new()),
    )
    .unwrap_err();

    assert!(error.to_string().contains("part descriptors for 1 volumes"));
}

#[test]
fn extract_slot_override_keeps_two_ready_shards_per_slot() {
    let one_slot = TaskPoolConfig::with_extract_slots(1);
    assert_eq!(one_slot.extract_slots, 1);
    assert_eq!(one_slot.extract_shards, 2);

    let two_slots = TaskPoolConfig::with_extract_slots(2);
    assert_eq!(two_slots.extract_slots, 2);
    assert_eq!(two_slots.extract_shards, 4);
}
