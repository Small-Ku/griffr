use super::*;

#[test]
fn full_archive_commits_verified_entries_and_skips_owned_paths() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    std::fs::write(install_dir.join("task-owned.bin"), b"owned by task").unwrap();

    let zip_path = tmp.path().join("full.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("archive.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"archive output").unwrap();
    zip.start_file("task-owned.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"must not be extracted").unwrap();
    zip.finish().unwrap();
    std::fs::copy(&zip_path, source_dir.join("full.zip.001")).unwrap();

    let expected = archive_expected_files(vec![
        crate::api::types::GameFileEntry {
            path: "archive.bin".to_string(),
            md5: crate::to_hex(&Md5::digest(b"archive output")),
            size: 14,
        },
        crate::api::types::GameFileEntry {
            path: "task-owned.bin".to_string(),
            // Deliberately invalid. The archive branch must not read or verify
            // a path whose final writer is an independent file task.
            md5: "00000000000000000000000000000000".to_string(),
            size: 21,
        },
    ]);
    let tasks = vec![Task::OpenArchive {
        base_name: "full".to_string(),
        source: ArchiveSource::Local(vec![source_dir.join("full.zip.001")]),
        dest: install_dir.clone(),
        retention: ArchiveRetention::KeepFullVolumes,
        password: None,
        patch_options: crate::runtime::PatchApplyOptions::default(),
        expected_files: expected,
        excluded_commit_paths: std::sync::Arc::new(std::collections::BTreeSet::from([
            "task-owned.bin".to_string(),
        ])),
    }];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();

    assert!(result
        .outcomes
        .iter()
        .all(|outcome| !matches!(outcome, TaskOutcome::Failed { .. })));
    assert_eq!(
        std::fs::read(install_dir.join("archive.bin")).unwrap(),
        b"archive output"
    );
    assert_eq!(
        std::fs::read(install_dir.join("task-owned.bin")).unwrap(),
        b"owned by task"
    );
    assert!(result.outcomes.iter().any(|outcome| matches!(
        outcome,
        TaskOutcome::Changed { path } if path == "archive.bin"
    )));
    assert!(!result.outcomes.iter().any(|outcome| matches!(
        outcome,
        TaskOutcome::Changed { path } if path == "task-owned.bin"
    )));
}

#[test]
fn full_archive_applies_deferred_delete_after_payload_commits() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    std::fs::write(install_dir.join("obsolete.bin"), b"remove after success").unwrap();

    let zip_path = tmp.path().join("forward-delete.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("payload.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"new payload").unwrap();
    zip.start_file("delete_files.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"obsolete.bin\n").unwrap();
    zip.finish().unwrap();
    std::fs::copy(&zip_path, source_dir.join("forward-delete.zip.001")).unwrap();

    let tasks = vec![Task::OpenArchive {
        base_name: "forward-delete".to_string(),
        source: ArchiveSource::Local(vec![source_dir.join("forward-delete.zip.001")]),
        dest: install_dir.clone(),
        retention: ArchiveRetention::Ephemeral,
        password: None,
        patch_options: crate::runtime::PatchApplyOptions::default(),
        expected_files: archive_expected_files(vec![crate::api::types::GameFileEntry {
            path: "payload.bin".to_string(),
            md5: crate::to_hex(&Md5::digest(b"new payload")),
            size: 11,
        }]),
        excluded_commit_paths: std::sync::Arc::new(std::collections::BTreeSet::new()),
    }];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();

    assert!(result
        .outcomes
        .iter()
        .all(|outcome| !matches!(outcome, TaskOutcome::Failed { .. })));
    assert_eq!(
        std::fs::read(install_dir.join("payload.bin")).unwrap(),
        b"new payload"
    );
    assert!(!install_dir.join("obsolete.bin").exists());
    assert!(!install_dir.join("delete_files.txt").exists());
}

#[test]
fn full_archive_keeps_earlier_commits_when_a_later_entry_fails() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    std::fs::write(install_dir.join("first.bin"), b"old first").unwrap();
    std::fs::write(install_dir.join("second.bin"), b"old second").unwrap();
    std::fs::write(install_dir.join("obsolete.bin"), b"keep on failed run").unwrap();

    let zip_path = tmp.path().join("forward.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("first.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"new first").unwrap();
    zip.start_file("second.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"new second").unwrap();
    zip.start_file("delete_files.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"obsolete.bin\n").unwrap();
    zip.finish().unwrap();
    std::fs::copy(&zip_path, source_dir.join("forward.zip.001")).unwrap();

    let expected = archive_expected_files(vec![
        crate::api::types::GameFileEntry {
            path: "first.bin".to_string(),
            md5: crate::to_hex(&Md5::digest(b"new first")),
            size: 9,
        },
        crate::api::types::GameFileEntry {
            path: "second.bin".to_string(),
            md5: "00000000000000000000000000000000".to_string(),
            size: 10,
        },
    ]);
    let tasks = vec![Task::OpenArchive {
        base_name: "forward".to_string(),
        source: ArchiveSource::Local(vec![source_dir.join("forward.zip.001")]),
        dest: install_dir.clone(),
        retention: ArchiveRetention::Ephemeral,
        password: None,
        patch_options: crate::runtime::PatchApplyOptions::default(),
        expected_files: expected,
        excluded_commit_paths: std::sync::Arc::new(std::collections::BTreeSet::new()),
    }];
    let config = TaskPoolConfig {
        extract_slots: 1,
        extract_shards: 1,
        ..Default::default()
    };

    let result = run_tasks(tasks, config).unwrap();

    assert!(result
        .outcomes
        .iter()
        .any(|outcome| matches!(outcome, TaskOutcome::Failed { .. })));
    assert_eq!(
        std::fs::read(install_dir.join("first.bin")).unwrap(),
        b"new first"
    );
    assert_eq!(
        std::fs::read(install_dir.join("second.bin")).unwrap(),
        b"old second"
    );
    assert_eq!(
        std::fs::read(install_dir.join("obsolete.bin")).unwrap(),
        b"keep on failed run"
    );
    assert!(!install_dir.join("delete_files.txt").exists());
}

#[test]
fn extract_task_spawns_vfs_patch_and_delete_manifest_follow_up_tasks() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(install_dir.join("Endfield_Data/Plugins/x86_64")).unwrap();
    std::fs::write(
        install_dir.join("Endfield_Data/Plugins/x86_64/libHAPI.dll"),
        b"obsolete",
    )
    .unwrap();

    let zip_path = tmp.path().join("bundle.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("payload.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"updated payload").unwrap();
    zip.start_file("patch.json", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(
        br#"{
  "version": "75.0.0",
  "vfs_base_path": "Arknights_Data/StreamingAssets/AB/Windows",
  "files": [
    {
      "name": "ui/direct.ab",
      "md5": "75c4e133155014e946c3ef39652b0ba8",
      "size": 13,
      "local_path": "files/ui/direct.ab",
      "diffType": 0,
      "patch": []
    }
  ]
}"#,
    )
    .unwrap();
    zip.start_file("vfs_files/files/ui/direct.ab", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"patched bytes").unwrap();
    zip.start_file("delete_files.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"Endfield_Data/Plugins/x86_64/libHAPI.dll\n")
        .unwrap();
    zip.finish().unwrap();

    let zip_bytes = std::fs::read(&zip_path).unwrap();
    std::fs::write(source_dir.join("bundle.zip.001"), &zip_bytes).unwrap();

    let tasks = vec![Task::OpenArchive {
        base_name: "bundle".to_string(),
        source: ArchiveSource::Local(vec![source_dir.join("bundle.zip.001")]),
        dest: install_dir.clone(),
        retention: ArchiveRetention::KeepFullVolumes,
        password: None,
        patch_options: crate::runtime::PatchApplyOptions::default(),
        expected_files: crate::runtime::task_pool::archive_expected_files(Vec::new()),
        excluded_commit_paths: std::sync::Arc::new(std::collections::BTreeSet::new()),
    }];

    let (progress_sender, progress_receiver) = crate::runtime::ProgressSender::channel();
    let progress = TaskProgress::new(progress_sender)
        .with_commit(crate::runtime::ProgressLane::ARCHIVE_COMMIT)
        .with_patch(crate::runtime::ProgressLane::ARCHIVE_PATCH)
        .with_delete(crate::runtime::ProgressLane::ARCHIVE_DELETE);
    let result = run_tasks_with_progress(tasks, TaskPoolConfig::default(), progress).unwrap();
    let mut progress_updates = Vec::new();
    while let Some(update) = progress_receiver.try_recv() {
        progress_updates.push(update);
    }

    assert!(
        result
            .outcomes
            .iter()
            .all(|event| !matches!(event, TaskOutcome::Failed { .. })),
        "extract + delete manifest task should finish without failures: {:?}",
        result.outcomes
    );
    assert!(progress_updates.iter().any(|update| matches!(
        update,
        crate::runtime::ProgressUpdate::Advanced {
            lane: crate::runtime::ProgressLane::ARCHIVE_COMMIT,
            finished,
            total: Some(total),
            ..
        } if finished == total && *total > 0
    )));
    assert!(progress_updates.iter().any(|update| matches!(
        update,
        crate::runtime::ProgressUpdate::Advanced {
            lane: crate::runtime::ProgressLane::ARCHIVE_PATCH,
            finished: 1,
            total: Some(1),
            ..
        }
    )));
    assert!(progress_updates.iter().any(|update| matches!(
        update,
        crate::runtime::ProgressUpdate::Advanced {
            lane: crate::runtime::ProgressLane::ARCHIVE_DELETE,
            finished: 1,
            total: Some(1),
            ..
        }
    )));
    assert_eq!(
        std::fs::read_to_string(install_dir.join("payload.txt")).unwrap(),
        "updated payload"
    );
    assert_eq!(
        std::fs::read(install_dir.join("Arknights_Data/StreamingAssets/AB/Windows/ui/direct.ab"))
            .unwrap(),
        b"patched bytes"
    );
    assert!(!install_dir
        .join("Endfield_Data/Plugins/x86_64/libHAPI.dll")
        .exists());
    assert!(!install_dir.join("delete_files.txt").exists());
    assert!(!install_dir.join("patch.json").exists());
    assert!(!install_dir.join("vfs_files").exists());
}

#[test]
fn patch_archive_extracts_only_selected_payloads_and_direct_entries() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("downloads");
    let install_dir = tmp.path().join("install");
    let existing_vfs = install_dir.join("Arknights_Data/StreamingAssets/AB/Windows/ui/already.ab");
    std::fs::create_dir_all(existing_vfs.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(&existing_vfs, b"already correct").unwrap();
    std::fs::write(install_dir.join("task-owned.bin"), b"owned by task").unwrap();

    let zip_path = tmp.path().join("selected-patch.zip");
    let zip_file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(zip_file);
    zip.start_file("payload.txt", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"updated payload").unwrap();
    zip.start_file("task-owned.bin", FileOptions::<()>::default())
        .unwrap();
    zip.write_all(b"must stay excluded").unwrap();
    zip.start_file("patch.json", FileOptions::<()>::default())
        .unwrap();
    let existing_md5 = crate::to_hex(&Md5::digest(b"already correct"));
    zip.write_all(
        format!(
            r#"{{
  "version": "75.0.0",
  "vfs_base_path": "Arknights_Data/StreamingAssets/AB/Windows",
  "files": [
    {{
      "name": "ui/already.ab",
      "md5": "{existing_md5}",
      "size": 15,
      "local_path": "files/ui/already.ab",
      "diffType": 0,
      "patch": []
    }}
  ]
}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let unused_payload = b"unused corrupt candidate";
    zip.start_file(
        "vfs_files/files/ui/already.ab",
        FileOptions::<()>::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(unused_payload).unwrap();
    zip.finish().unwrap();

    let mut zip_bytes = std::fs::read(&zip_path).unwrap();
    let payload_offset = zip_bytes
        .windows(unused_payload.len())
        .position(|window| window == unused_payload)
        .expect("stored payload bytes should be present in ZIP");
    zip_bytes[payload_offset] ^= 0xff;
    std::fs::write(source_dir.join("selected-patch.zip.001"), zip_bytes).unwrap();

    let tasks = vec![Task::OpenArchive {
        base_name: "selected-patch".to_string(),
        source: ArchiveSource::Local(vec![source_dir.join("selected-patch.zip.001")]),
        dest: install_dir.clone(),
        retention: ArchiveRetention::Ephemeral,
        password: None,
        patch_options: crate::runtime::PatchApplyOptions::default(),
        expected_files: archive_expected_files(vec![crate::api::types::GameFileEntry {
            path: "payload.txt".to_string(),
            md5: crate::to_hex(&Md5::digest(b"updated payload")),
            size: 15,
        }]),
        excluded_commit_paths: std::sync::Arc::new(std::collections::BTreeSet::from([
            "task-owned.bin".to_string(),
        ])),
    }];

    let result = run_tasks(tasks, TaskPoolConfig::default()).unwrap();

    assert!(
        result
            .outcomes
            .iter()
            .all(|outcome| !matches!(outcome, TaskOutcome::Failed { .. })),
        "unused corrupt payload must not enter extraction: {:?}",
        result.outcomes
    );
    assert_eq!(
        std::fs::read_to_string(install_dir.join("payload.txt")).unwrap(),
        "updated payload"
    );
    assert_eq!(std::fs::read(&existing_vfs).unwrap(), b"already correct");
    assert_eq!(
        std::fs::read(install_dir.join("task-owned.bin")).unwrap(),
        b"owned by task"
    );
}
