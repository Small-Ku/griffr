use super::*;

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

    let tasks = vec![Task::Extract {
        base_name: "bundle".to_string(),
        volumes: vec![source_dir.join("bundle.zip.001")],
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
